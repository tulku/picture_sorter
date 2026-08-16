//! File-role classification for camera cards.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Raw,
    Jpeg,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    Media(MediaKind),
    Sidecar(MediaKind),
    SidecarUnknown,
    Ignore,
}

pub fn dest_tree(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Raw => "RAW",
        MediaKind::Jpeg => "JPEG",
        MediaKind::Video => "VIDEO",
    }
}

const RAW_EXTS: &[&str] = &["cr2", "nef", "arw", "dng", "raw", "orf", "ori"];
const JPEG_EXTS: &[&str] = &["jpg", "jpeg", "tif", "tiff", "png"];
const VIDEO_EXTS: &[&str] = &["mov", "mp4", "avi", "mts", "m4v", "mpg", "mpeg"];
const SIDECAR_EXTS: &[&str] = &["xmp", "rrdata", "rrexif", "orig", "pp3", "dop", "on1"];

fn ext_kind(ext: &str) -> Option<MediaKind> {
    if RAW_EXTS.contains(&ext) {
        Some(MediaKind::Raw)
    } else if JPEG_EXTS.contains(&ext) {
        Some(MediaKind::Jpeg)
    } else if VIDEO_EXTS.contains(&ext) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

fn is_sidecar_ext(ext: &str) -> bool {
    SIDECAR_EXTS.contains(&ext)
}

pub fn classify_name(filename: &str) -> FileRole {
    if filename.starts_with('.') {
        return FileRole::Ignore;
    }
    let lower = filename.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('.').collect();
    if parts.len() < 2 || parts[0].is_empty() {
        return FileRole::Ignore;
    }
    let last = parts[parts.len() - 1];
    if let Some(kind) = ext_kind(last) {
        return FileRole::Media(kind);
    }
    if is_sidecar_ext(last) {
        if parts.len() >= 3 {
            if let Some(kind) = ext_kind(parts[parts.len() - 2]) {
                return FileRole::Sidecar(kind);
            }
        }
        return FileRole::SidecarUnknown;
    }
    FileRole::Ignore
}

pub fn group_stem(filename: &str) -> Option<&str> {
    if matches!(classify_name(filename), FileRole::Ignore) {
        return None;
    }
    filename.split('.').next().filter(|s| !s.is_empty())
}

pub fn is_photo_media(role: FileRole) -> bool {
    matches!(role, FileRole::Media(_))
}

pub fn is_raw_file(filename: &str) -> bool {
    matches!(classify_name(filename), FileRole::Media(MediaKind::Raw))
}

pub fn is_jpeg_file(filename: &str) -> bool {
    matches!(classify_name(filename), FileRole::Media(MediaKind::Jpeg))
}

pub fn is_video_file(filename: &str) -> bool {
    matches!(classify_name(filename), FileRole::Media(MediaKind::Video))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn olympus_raw_and_high_res_go_to_raw() {
        assert_eq!(classify_name("P5178617.ORF"), FileRole::Media(MediaKind::Raw));
        assert_eq!(classify_name("P5178617.ori"), FileRole::Media(MediaKind::Raw));
        assert_eq!(dest_tree(MediaKind::Raw), "RAW");
    }

    #[test]
    fn jpeg_and_still_exports_go_to_jpeg() {
        assert_eq!(classify_name("P5178617.JPG"), FileRole::Media(MediaKind::Jpeg));
        assert_eq!(classify_name("export.tiff"), FileRole::Media(MediaKind::Jpeg));
        assert_eq!(classify_name("export.PNG"), FileRole::Media(MediaKind::Jpeg));
    }

    #[test]
    fn movies_are_video() {
        assert_eq!(classify_name("P1075977.MOV"), FileRole::Media(MediaKind::Video));
        assert_eq!(classify_name("clip.mp4"), FileRole::Media(MediaKind::Video));
        assert_eq!(dest_tree(MediaKind::Video), "VIDEO");
    }

    #[test]
    fn sidecars_follow_the_media_extension() {
        assert_eq!(
            classify_name("P5157579.ORF.rrdata"),
            FileRole::Sidecar(MediaKind::Raw)
        );
        assert_eq!(
            classify_name("P5157579.JPG.orig"),
            FileRole::Sidecar(MediaKind::Jpeg)
        );
        assert_eq!(classify_name("P5157579.xmp"), FileRole::SidecarUnknown);
    }

    #[test]
    fn card_junk_and_dotfiles_are_ignored() {
        assert_eq!(classify_name("ALB27ZY1.BIN"), FileRole::Ignore);
        assert_eq!(classify_name("thumbs.db"), FileRole::Ignore);
        assert_eq!(classify_name(".hidden.ORF"), FileRole::Ignore);
        assert_eq!(classify_name("digikam4.db"), FileRole::Ignore);
        assert_eq!(group_stem(".hidden.ORF"), None);
    }

    #[test]
    fn group_stem_is_text_before_first_dot() {
        assert_eq!(group_stem("P5157579.ORF.rrdata"), Some("P5157579"));
        assert_eq!(group_stem("P5157579.JPG"), Some("P5157579"));
    }
}
