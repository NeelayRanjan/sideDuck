use super::*;
use serde_json::json;
use std::process::Command;

fn manifest() -> Manifest {
    Manifest {
        tile_size: 4,
        ..Manifest::new([16, 16])
    }
}
fn options() -> DocumentOptions {
    DocumentOptions {
        tile_cache_bytes: 64,
        checkpoint_interval: 3,
        ..Default::default()
    }
}
fn address(layer: u32, x: u32) -> TileAddress {
    TileAddress { layer, x, y: 0 }
}
fn pixels(value: u8) -> Vec<u8> {
    [value, 20, 40, 255].repeat(16)
}
fn create(path: &Path) -> Document {
    let mut doc = Document::create(path, manifest(), options()).unwrap();
    doc.apply(
        Operation::CreateLayer {
            layer: Layer::raster(0, "Paint"),
            index: 0,
        },
        [],
    )
    .unwrap();
    doc
}
fn paint(doc: &mut Document, value: u8) -> Commit {
    doc.apply(
        Operation::Stroke(StrokeRecord {
            version: 1,
            layer: 0,
            stroke_id: doc.sequence(),
            seed: 42,
            samples: json!([[1.0, 2.0, 0.5, 0.24494997439999997]]),
            brush: json!({"size":8.0}),
        }),
        [TileEdit {
            address: address(0, 0),
            pixels: Some(pixels(value)),
        }],
    )
    .unwrap()
}

#[test]
fn document_package_round_trip_ordered_content_addresses_and_exclusive_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image.pond");
    let mut doc = create(&path);
    let first = paint(&mut doc, 75);
    let second = paint(&mut doc, 95);
    assert_ne!(first.content_id, second.content_id);
    assert_eq!(second.sequence, first.sequence + 1);
    assert_eq!(doc.operation_ids().unwrap().len(), 4);
    assert!(matches!(
        Document::open(&path, options()),
        Err(Error::AlreadyOpen)
    ));
    let state = doc.state().clone();
    drop(doc);
    let mut reopened = Document::open(&path, options()).unwrap();
    assert_eq!(reopened.manifest(), &manifest());
    assert_eq!(reopened.state(), &state);
    assert_eq!(
        reopened.read_tile(address(0, 0)).unwrap().unwrap(),
        pixels(95)
    );
    assert!(reopened.tile_stats().resident_bytes <= 64);
    assert!(path.join("manifest.json").is_file());
    assert!(path.join("ops.log").is_file());
    assert!(path.join("checkpoints").is_dir());
}

#[test]
fn undo_redo_restores_pixels_structural_operations_and_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let mut doc = create(directory.path());
    paint(&mut doc, 10);
    paint(&mut doc, 90);
    doc.undo().unwrap();
    assert_eq!(doc.read_tile(address(0, 0)).unwrap().unwrap(), pixels(10));
    doc.redo().unwrap();
    assert_eq!(doc.read_tile(address(0, 0)).unwrap().unwrap(), pixels(90));
    doc.apply(Operation::RemoveLayer { id: 0 }, []).unwrap();
    assert!(doc.state().layers.is_empty());
    assert!(doc.read_tile(address(0, 0)).unwrap().is_none());
    drop(doc);
    let mut doc = Document::open(directory.path(), options()).unwrap();
    doc.undo().unwrap();
    assert_eq!(doc.state().layers.len(), 1);
    assert_eq!(doc.read_tile(address(0, 0)).unwrap().unwrap(), pixels(90));
    doc.undo().unwrap();
    assert_eq!(doc.read_tile(address(0, 0)).unwrap().unwrap(), pixels(10));
    paint(&mut doc, 30);
    assert!(!doc.can_redo());
    assert!(matches!(doc.redo(), Err(Error::NoRedo)));
}

#[test]
fn deterministic_replay_from_checkpoint_reproduces_document_bit_identically() {
    let directory = tempfile::tempdir().unwrap();
    let mut doc = create(&directory.path().join("source"));
    paint(&mut doc, 10);
    let checkpoint = doc.checkpoint().unwrap().sequence;
    paint(&mut doc, 80);
    doc.apply(
        Operation::RasterEdit {
            layers: vec![0],
            label: "Fill".into(),
        },
        [TileEdit {
            address: address(0, 1),
            pixels: Some(pixels(60)),
        }],
    )
    .unwrap();
    doc.undo().unwrap();
    doc.redo().unwrap();
    let mut replay = doc
        .replay_tiles_from_checkpoint(checkpoint, directory.path().join("replay"))
        .unwrap();
    assert_eq!(replay.state, *doc.state());
    assert_eq!(replay.replayed_operations, 4);
    let keys: Vec<_> = doc.tile_keys().collect();
    assert_eq!(
        replay.tiles.keys().collect::<BTreeSet<_>>(),
        keys.iter().copied().map(Into::into).collect()
    );
    for key in keys {
        assert_eq!(
            replay.tiles.get(key.into()).unwrap(),
            doc.read_tile(key).unwrap()
        );
    }
}

#[test]
fn failed_tile_or_metadata_mutation_rolls_back_without_partial_document_changes() {
    let directory = tempfile::tempdir().unwrap();
    let mut doc = create(directory.path());
    paint(&mut doc, 10);
    let before = doc.sequence();
    let bad = vec![
        TileEdit {
            address: address(0, 0),
            pixels: Some(pixels(90)),
        },
        TileEdit {
            address: address(0, 1),
            pixels: Some(vec![1]),
        },
    ];
    assert!(doc
        .apply(
            Operation::RasterEdit {
                layers: vec![0],
                label: "Bad".into()
            },
            bad
        )
        .is_err());
    assert_eq!(doc.sequence(), before);
    assert_eq!(doc.read_tile(address(0, 0)).unwrap().unwrap(), pixels(10));
    let mut layer = doc.state().layers[0].clone();
    layer.opacity = f32::NAN;
    assert!(doc.apply(Operation::SetLayer { layer }, []).is_err());
    assert_eq!(doc.sequence(), before);
    assert!(doc.read_tile(address(0, 4)).is_err());
}

#[test]
fn groups_masks_clipping_reorder_lock_merge_and_selection_operations_validate() {
    let directory = tempfile::tempdir().unwrap();
    let mut doc = create(directory.path());
    doc.apply(
        Operation::CreateLayer {
            layer: Layer::raster(1, "Mask"),
            index: 1,
        },
        [],
    )
    .unwrap();
    let mut group = Layer::raster(2, "Group");
    group.kind = LayerKind::Group { isolated: true };
    doc.apply(
        Operation::CreateLayer {
            layer: group,
            index: 2,
        },
        [],
    )
    .unwrap();
    let mut child = Layer::raster(3, "Child");
    child.parent = Some(2);
    child.mask = Some(1);
    doc.apply(
        Operation::CreateLayer {
            layer: child,
            index: 3,
        },
        [],
    )
    .unwrap();
    let mut clip = Layer::raster(4, "Clip");
    clip.parent = Some(2);
    clip.clip_base = Some(3);
    doc.apply(
        Operation::CreateLayer {
            layer: clip,
            index: 4,
        },
        [],
    )
    .unwrap();
    assert!(doc
        .apply(
            Operation::ReorderLayer {
                id: 4,
                index: 0,
                parent: Some(2)
            },
            []
        )
        .is_err());
    let mut locked = doc.state().layers[0].clone();
    locked.locked = true;
    doc.apply(Operation::SetLayer { layer: locked }, [])
        .unwrap();
    assert!(doc
        .apply(
            Operation::RasterEdit {
                layers: vec![0],
                label: "Fill".into()
            },
            []
        )
        .is_err());
    doc.undo().unwrap();
    doc.apply(
        Operation::SetSelection {
            selection: Some(json!({"version":1,"shape":"rectangle","bounds":[0,0,4,4]})),
        },
        [],
    )
    .unwrap();
    assert!(doc.state().selection.is_some());
    doc.apply(
        Operation::Merge {
            sources: vec![0, 1],
            target: 0,
        },
        [TileEdit {
            address: address(0, 0),
            pixels: Some(pixels(45)),
        }],
    )
    .unwrap();
    assert!(doc.state().layer(1).is_none());
    assert_eq!(doc.state().layer(3).unwrap().mask, None);
    doc.apply(Operation::RemoveLayer { id: 2 }, []).unwrap();
    assert!(doc.state().layer(3).is_none());
    assert!(doc.state().layer(4).is_none());
    assert!(doc.next_layer_id().unwrap() > 4);
}

#[test]
fn bounded_undo_history_and_transparent_float_tiles_remain_sparse() {
    let directory = tempfile::tempdir().unwrap();
    let opts = DocumentOptions {
        undo_budget_bytes: 1,
        ..options()
    };
    let mut doc = Document::create(
        directory.path(),
        Manifest {
            tile_format: TileFormat::Rgba16Float,
            ..manifest()
        },
        DocumentOptions {
            tile_cache_bytes: 128,
            ..opts
        },
    )
    .unwrap();
    doc.apply(
        Operation::CreateLayer {
            layer: Layer::raster(0, "Float"),
            index: 0,
        },
        [],
    )
    .unwrap();
    assert!(!doc.can_undo());
    assert!(matches!(doc.undo(), Err(Error::NoUndo)));
    let bytes = [0, 0x34, 0, 0x34, 0, 0x34, 0, 0x80].repeat(16);
    doc.apply(
        Operation::RasterEdit {
            layers: vec![0],
            label: "Clear".into(),
        },
        [TileEdit {
            address: address(0, 0),
            pixels: Some(bytes),
        }],
    )
    .unwrap();
    assert_eq!(doc.tile_keys().count(), 0);
    assert!(doc.undo_retained_bytes() <= 1);
}

#[test]
fn torn_log_tail_is_discarded_and_committed_index_is_restored() {
    let directory = tempfile::tempdir().unwrap();
    let mut doc = create(directory.path());
    paint(&mut doc, 10);
    let sequence = doc.sequence();
    doc.tiles
        .as_mut()
        .unwrap()
        .put(address(0, 0).into(), &pixels(99))
        .unwrap();
    doc.tiles.as_mut().unwrap().commit().unwrap();
    doc.log.seek(SeekFrom::End(0)).unwrap();
    doc.log.write_all(&100_u32.to_le_bytes()).unwrap();
    doc.log.write_all(b"torn").unwrap();
    doc.log.sync_all().unwrap();
    drop(doc);
    let mut recovered = Document::open(directory.path(), options()).unwrap();
    assert_eq!(recovered.sequence(), sequence);
    assert_eq!(
        recovered.read_tile(address(0, 0)).unwrap().unwrap(),
        pixels(10)
    );
    paint(&mut recovered, 20);
    assert_eq!(recovered.sequence(), sequence + 1);
}

#[test]
fn committed_content_corruption_and_hostile_lengths_are_rejected_before_allocation() {
    let directory = tempfile::tempdir().unwrap();
    let doc = create(directory.path());
    drop(doc);
    let mut log = OpenOptions::new()
        .write(true)
        .open(directory.path().join("ops.log"))
        .unwrap();
    log.seek(SeekFrom::Start(8)).unwrap();
    log.write_all(&u32::MAX.to_le_bytes()).unwrap();
    drop(log);
    assert!(matches!(
        Document::open(directory.path(), options()),
        Err(Error::Limit(_))
    ));
    let directory = tempfile::tempdir().unwrap();
    let doc = create(directory.path());
    let sequence = doc.sequence();
    drop(doc);
    let mut index = OpenOptions::new()
        .write(true)
        .open(storage::index_path(directory.path(), sequence))
        .unwrap();
    index.write_all(b"broken").unwrap();
    drop(index);
    assert!(matches!(
        Document::open(directory.path(), options()),
        Err(Error::Corrupt("committed tile index checksum"))
    ));
}

#[test]
fn crash_child() {
    let Ok(path) = std::env::var("DOCUMENT_CRASH_PATH") else {
        return;
    };
    let stage: usize = std::env::var("DOCUMENT_CRASH_STAGE")
        .unwrap()
        .parse()
        .unwrap();
    let mut doc = Document::open(path, options()).unwrap();
    let mut observed = 0;
    let _ = doc.apply_with_observer(
        Operation::RasterEdit {
            layers: vec![0],
            label: "Fault injection".into(),
        },
        [TileEdit {
            address: address(0, 0),
            pixels: Some(pixels(90)),
        }],
        &mut |_| {
            if observed == stage {
                std::process::exit(91);
            }
            observed += 1;
            Ok(())
        },
    );
    panic!("requested crash point was not reached");
}

#[test]
fn source_assets_and_atomic_png_thumbnail_round_trip_with_integrity_checks() {
    let directory = tempfile::tempdir().unwrap();
    let doc = create(directory.path());
    let source = b"%PDF-1.7 retained original source bytes";
    let id = doc.put_source(source).unwrap();
    assert_eq!(doc.put_source(source).unwrap(), id);
    assert_eq!(doc.read_source(id).unwrap(), source);
    doc.set_thumbnail_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128])
        .unwrap();
    let decoder = png::Decoder::new(File::open(directory.path().join("thumbnail.png")).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut output = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut output).unwrap();
    assert_eq!((info.width, info.height), (2, 1));
    assert_eq!(output, [255, 0, 0, 255, 0, 255, 0, 128]);
    assert!(doc.set_thumbnail_rgba(0, 1, &[]).is_err());
    let path = directory
        .path()
        .join("objects")
        .join(format!("{}.source", storage::hex(id)));
    fs::write(path, b"corrupt").unwrap();
    assert!(matches!(doc.read_source(id), Err(Error::Corrupt(_))));
}

#[test]
fn bounded_operation_serialization_rejects_large_payload_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let mut doc = create(directory.path());
    doc.options.max_record_bytes = 2048;
    assert!(matches!(
        doc.apply(
            Operation::SetSelection {
                selection: Some(json!({"data":"x".repeat(4096)}))
            },
            []
        ),
        Err(Error::Limit(_))
    ));
    assert_eq!(doc.sequence(), 1);
    assert!(doc.state().selection.is_none());
    let bytes = storage::json_bytes(&json!({"x":7}), 7).unwrap();
    assert_eq!(bytes, br#"{"x":7}"#);
}

#[test]
fn kill_mid_stroke_at_every_commit_boundary_leaves_document_openable_and_correct() {
    for stage in 0..5 {
        let directory = tempfile::tempdir().unwrap();
        let mut doc = create(directory.path());
        paint(&mut doc, 10);
        let before = doc.sequence();
        drop(doc);
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::crash_child", "--nocapture"])
            .env("DOCUMENT_CRASH_PATH", directory.path())
            .env("DOCUMENT_CRASH_STAGE", stage.to_string())
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(91),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut recovered = Document::open(directory.path(), options()).unwrap();
        let committed = stage == 4;
        assert_eq!(recovered.sequence(), before + u64::from(committed));
        assert_eq!(
            recovered.read_tile(address(0, 0)).unwrap().unwrap(),
            pixels(if committed { 90 } else { 10 })
        );
    }
}
