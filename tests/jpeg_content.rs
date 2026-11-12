use photo_sorter::{domain::Digest, import::inspect_jpeg};
use std::io::Cursor;

const RED: &[u8] = include_bytes!("fixtures/jpeg/red.jpg");
const BLUE: &[u8] = include_bytes!("fixtures/jpeg/blue.jpg");

fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut result = vec![0xff, marker];
    result.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    result.extend_from_slice(payload);
    result
}
fn insert(image: &[u8], blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut result = image[..2].to_vec();
    for block in blocks {
        result.extend_from_slice(block);
    }
    result.extend_from_slice(&image[2..]);
    result
}
fn exif(orientation: u16) -> Vec<u8> {
    let mut data = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
    data.extend_from_slice(&orientation.to_le_bytes());
    data.extend_from_slice(&[0; 6]);
    segment(0xe1, &data)
}
#[test]
fn gps_xmp_and_comments_do_not_change_image_identity() {
    let camera = insert(RED, &[exif(1)]);
    let geotagged = insert(
        RED,
        &[
            exif(1),
            block_xmp(
                "<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:exif='http://ns.adobe.com/exif/1.0/' exif:GPSLatitude='51,12N'/></rdf:RDF></x:xmpmeta>",
            ),
            segment(0xfe, b"geotagged"),
        ],
    );
    let first = inspect_jpeg(Cursor::new(&camera)).unwrap();
    let second = inspect_jpeg(Cursor::new(&geotagged)).unwrap();
    assert!(first.image.is_some());
    assert_eq!(first.image, second.image);
    assert_eq!(first.digest, Digest::of(&camera));
    assert_eq!(second.digest, Digest::of(&geotagged));
    assert_ne!(first.digest, second.digest);
    assert_eq!(second.size, geotagged.len() as u64);
}
#[test]
fn changed_pixels_orientation_and_colour_profiles_remain_distinct() {
    let plain = inspect_jpeg(Cursor::new(RED)).unwrap().image.unwrap();
    for image in [
        BLUE.to_vec(),
        insert(RED, &[exif(6)]),
        insert(
            RED,
            &[segment(0xe2, b"ICC_PROFILE\0\x01\x01changed profile")],
        ),
    ] {
        assert_ne!(
            Some(plain.clone()),
            inspect_jpeg(Cursor::new(image)).unwrap().image
        );
    }
}
#[test]
fn malformed_or_unsupported_jpeg_retains_only_exact_identity() {
    let mut truncated = RED.to_vec();
    truncated.truncate(truncated.len() - 2);
    let bad_exif = insert(RED, &[segment(0xe1, b"Exif\0\0II\x2a\0\xff\xff\xff\xff")]);
    for image in [
        b"not a JPEG".to_vec(),
        truncated,
        bad_exif,
        vec![0xff, 0xd8, 0xff, 0xe1, 0xff, 0xff],
    ] {
        let result = inspect_jpeg(Cursor::new(&image)).unwrap();
        assert_eq!(result.image, None);
        assert_eq!(result.digest, Digest::of(&image));
        assert_eq!(result.size, image.len() as u64);
    }
}
#[test]
fn unknown_application_data_is_part_of_image_identity() {
    let plain = inspect_jpeg(Cursor::new(RED)).unwrap().image.unwrap();
    let unknown = insert(RED, &[segment(0xe1, b"unknown application payload")]);
    assert_ne!(
        Some(plain),
        inspect_jpeg(Cursor::new(unknown)).unwrap().image
    );
}

#[test]
fn iptc_changes_are_ignored_but_other_photoshop_resources_are_retained() {
    fn photoshop(id: u16, data: &[u8]) -> Vec<u8> {
        let mut payload = b"Photoshop 3.0\0".to_vec();
        payload.extend_from_slice(b"8BIM");
        payload.extend_from_slice(&id.to_be_bytes());
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(&(data.len() as u32).to_be_bytes());
        payload.extend_from_slice(data);
        if data.len() % 2 == 1 {
            payload.push(0);
        }
        segment(0xed, &payload)
    }
    let plain = inspect_jpeg(Cursor::new(RED)).unwrap().image.unwrap();
    assert_eq!(
        Some(plain.clone()),
        inspect_jpeg(Cursor::new(insert(RED, &[photoshop(0x0404, b"caption")])))
            .unwrap()
            .image
    );
    assert_ne!(
        Some(plain),
        inspect_jpeg(Cursor::new(insert(
            RED,
            &[photoshop(0x040f, b"colour profile")]
        )))
        .unwrap()
        .image
    );
}
#[test]
fn coding_tables_and_unsupported_progressive_images_are_not_ignored() {
    let first = inspect_jpeg(Cursor::new(RED)).unwrap().image.unwrap();
    let mut changed = RED.to_vec();
    let table = changed.windows(2).position(|x| x == [0xff, 0xdb]).unwrap();
    changed[table + 5] = changed[table + 5].saturating_add(1);
    assert_ne!(
        Some(first),
        inspect_jpeg(Cursor::new(changed)).unwrap().image
    );
    let mut progressive = RED.to_vec();
    let frame = progressive
        .windows(2)
        .position(|x| x == [0xff, 0xc0])
        .unwrap();
    progressive[frame + 1] = 0xc2;
    assert_eq!(None, inspect_jpeg(Cursor::new(progressive)).unwrap().image);
}
#[test]
fn every_truncated_prefix_remains_exact_only() {
    for end in 0..RED.len() {
        let result = inspect_jpeg(Cursor::new(&RED[..end])).unwrap();
        assert_eq!(result.image, None, "truncated at {end}");
        assert_eq!(result.digest, Digest::of(&RED[..end]));
    }
}
#[test]
fn changed_source_entry_is_rejected_and_fingerprint_cache_is_exact_keyed() {
    use photo_sorter::{
        catalog::Catalog,
        fs::{RootHandle, RootPolicy},
        import::{Inventory, inspect_jpeg_source},
    };
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let state = directory.path().join("state");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&state).unwrap();
    std::fs::write(source.join("A.JPG"), RED).unwrap();
    let entry = Inventory::open(RootHandle::open(&source, RootPolicy::Local).unwrap())
        .unwrap()
        .next_batch(1)
        .unwrap()
        .remove(0);
    let receipt = inspect_jpeg_source(&entry).unwrap();
    let mut catalog = Catalog::open(&RootHandle::open(&state, RootPolicy::Local).unwrap()).unwrap();
    assert_eq!(catalog.jpeg_fingerprint(&receipt.digest).unwrap(), None);
    catalog.cache_jpeg_inspection(&receipt).unwrap();
    assert_eq!(
        catalog.jpeg_fingerprint(&receipt.digest).unwrap(),
        Some(receipt.image)
    );
    assert_eq!(catalog.jpeg_fingerprint(&Digest::of(BLUE)).unwrap(), None);
    std::fs::write(source.join("A.JPG"), BLUE).unwrap();
    assert!(inspect_jpeg_source(&entry).is_err());
}

#[test]
fn explicit_uncalibrated_colour_space_is_not_unspecified() {
    let mut data = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x69\x87\x04\0\x01\0\0\0\x1a\0\0\0\0\0\0\0\x01\0\x01\xa0\x03\0\x01\0\0\0".to_vec();
    data.extend_from_slice(&65535u16.to_le_bytes());
    data.extend_from_slice(&[0; 6]);
    let unspecified = inspect_jpeg(Cursor::new(RED)).unwrap().image.unwrap();
    let uncalibrated = inspect_jpeg(Cursor::new(insert(RED, &[segment(0xe1, &data)])))
        .unwrap()
        .image
        .unwrap();
    assert_ne!(unspecified, uncalibrated);
}

#[test]
fn jpeg_accounting_upgrade_preserves_existing_catalogue() {
    use photo_sorter::{
        catalog::Catalog,
        fs::{RootHandle, RootPolicy},
    };
    let state = tempfile::tempdir().unwrap();
    let root = RootHandle::open(state.path(), RootPolicy::Local).unwrap();
    let mut catalog = Catalog::open(&root).unwrap();
    let group = catalog.create_group(false).unwrap();
    drop(catalog);
    let db = rusqlite::Connection::open(state.path().join("catalog.sqlite")).unwrap();
    db.execute_batch("DROP TABLE jpeg_duplicates; DROP TABLE jpeg_checks; DROP TABLE jpeg_fingerprints; PRAGMA user_version=1;").unwrap();
    drop(db);
    let catalog = Catalog::open(&root).unwrap();
    assert_eq!(catalog.group(group).unwrap().id, group);
    drop(catalog);
    let db = rusqlite::Connection::open(state.path().join("catalog.sqlite")).unwrap();
    let version: u64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(
        version, 2,
        "old readers must reject new duplicate-accounting states"
    );
    let duplicates: u64 = db
        .query_row("SELECT count(*) FROM jpeg_duplicates", [], |r| r.get(0))
        .unwrap();
    assert_eq!(duplicates, 0);
}

#[test]
fn xmp_only_rendering_changes_are_not_ignored() {
    let xmp = |rotation: u8| {
        block_xmp(&format!(
            "<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:tiff='http://ns.adobe.com/tiff/1.0/' tiff:Orientation='{rotation}'/></rdf:RDF></x:xmpmeta>"
        ))
    };
    let first = inspect_jpeg(Cursor::new(insert(RED, &[xmp(1)])))
        .unwrap()
        .image
        .unwrap();
    let rotated = inspect_jpeg(Cursor::new(insert(RED, &[xmp(6)])))
        .unwrap()
        .image
        .unwrap();
    assert_ne!(
        first, rotated,
        "XMP rotation affects displayed image identity"
    );
}
fn block_xmp(xml: &str) -> Vec<u8> {
    let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    payload.extend_from_slice(xml.as_bytes());
    segment(0xe1, &payload)
}

#[test]
fn exif_white_point_changes_remain_distinct() {
    let image = |numerator: u32| {
        let mut payload =
            b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x3e\x01\x05\0\x02\0\0\0\x1a\0\0\0\0\0\0\0".to_vec();
        for value in [numerator, 10000, 3290, 10000] {
            payload.extend_from_slice(&value.to_le_bytes());
        }
        inspect_jpeg(Cursor::new(insert(RED, &[segment(0xe1, &payload)])))
            .unwrap()
            .image
            .unwrap()
    };
    assert_ne!(image(3127), image(3457));
}
#[test]
fn xmp_namespace_prefixes_and_geotags_do_not_change_matching_rotation() {
    let first = block_xmp(
        "<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:tiff='http://ns.adobe.com/tiff/1.0/' tiff:Orientation='1'/></rdf:RDF></x:xmpmeta>",
    );
    let second = block_xmp(
        "<x:xmpmeta xmlns:x='adobe:ns:meta/'><r:RDF xmlns:r='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><r:Description xmlns:t='http://ns.adobe.com/tiff/1.0/' xmlns:e='http://ns.adobe.com/exif/1.0/' e:GPSLatitude='51,12N'><t:Orientation>1</t:Orientation></r:Description></r:RDF></x:xmpmeta>",
    );
    assert_eq!(
        inspect_jpeg(Cursor::new(insert(RED, &[first])))
            .unwrap()
            .image
            .unwrap(),
        inspect_jpeg(Cursor::new(insert(RED, &[second])))
            .unwrap()
            .image
            .unwrap()
    );
}
#[test]
fn unknown_xmp_rendering_is_preserved_and_unsafe_xml_is_unsupported() {
    let packet = |exposure: u8| {
        block_xmp(&format!(
            "<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:crs='http://ns.adobe.com/camera-raw-settings/1.0/' crs:Exposure2012='{exposure}'/></rdf:RDF></x:xmpmeta>"
        ))
    };
    assert_ne!(
        inspect_jpeg(Cursor::new(insert(RED, &[packet(0)])))
            .unwrap()
            .image
            .unwrap(),
        inspect_jpeg(Cursor::new(insert(RED, &[packet(1)])))
            .unwrap()
            .image
            .unwrap()
    );
    for xml in ["<!DOCTYPE x [<!ENTITY foo '6'>]><x/>", "<broken>"] {
        assert!(
            inspect_jpeg(Cursor::new(insert(RED, &[block_xmp(xml)])))
                .unwrap()
                .image
                .is_none()
        );
    }
    let extended = segment(0xe1, b"http://ns.adobe.com/xmp/extension/\0fragment");
    assert!(
        inspect_jpeg(Cursor::new(insert(RED, &[extended])))
            .unwrap()
            .image
            .is_none()
    );
}

#[test]
fn camera_xmp_trailing_zero_padding_does_not_disable_recognition() {
    let xml = "<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:tiff='http://ns.adobe.com/tiff/1.0/' tiff:Orientation='1'/></rdf:RDF></x:xmpmeta>";
    let clean = inspect_jpeg(Cursor::new(insert(RED, &[block_xmp(xml)])))
        .unwrap()
        .image
        .unwrap();
    let padded = inspect_jpeg(Cursor::new(insert(
        RED,
        &[block_xmp(&(xml.to_owned() + "\0"))],
    )))
    .unwrap()
    .image;
    assert_eq!(padded, Some(clean));
}
