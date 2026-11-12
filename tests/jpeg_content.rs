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
            segment(
                0xe1,
                b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta GPSLatitude='51.2'/ >",
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
