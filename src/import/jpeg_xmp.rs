use roxmltree::{Document, Node, ParsingOptions};
use serde::Serialize;
use std::collections::BTreeSet;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
const EXIF: &str = "http://ns.adobe.com/exif/1.0/";
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct XmpProperty {
    subject: String,
    namespace: String,
    name: String,
    value: String,
}
impl XmpProperty {
    pub(super) fn is_redundant_rendering(&self, orientation: u16, colour: Option<u16>) -> bool {
        let Some(value) = self
            .value
            .strip_prefix("s:")
            .and_then(|text| text.parse::<u16>().ok())
        else {
            return false;
        };
        (self.namespace == TIFF && self.name == "Orientation" && value == orientation)
            || (self.namespace == EXIF && self.name == "ColorSpace" && Some(value) == colour)
    }
}
fn inside_description(mut node: Node<'_, '_>) -> bool {
    while let Some(parent) = node.parent() {
        if parent.has_tag_name((RDF, "Description")) {
            return true;
        }
        node = parent;
    }
    false
}
fn descriptive(namespace: &str, name: &str) -> bool {
    match namespace {
        "http://purl.org/dc/elements/1.1/"
        | "http://ns.adobe.com/xap/1.0/"
        | "http://ns.adobe.com/xap/1.0/rights/"
        | "http://ns.adobe.com/xap/1.0/mm/"
        | "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/"
        | "http://iptc.org/std/Iptc4xmpExt/2008-02-29/" => true,
        TIFF => matches!(
            name,
            "Make"
                | "Model"
                | "Software"
                | "ImageDescription"
                | "Artist"
                | "Copyright"
                | "DateTime"
                | "DocumentName"
        ),
        EXIF | "http://cipa.jp/exif/1.0/" => {
            name.starts_with("GPS")
                || matches!(
                    name,
                    "DateTimeOriginal"
                        | "DateTimeDigitized"
                        | "UserComment"
                        | "ImageUniqueID"
                        | "ExifVersion"
                        | "FlashpixVersion"
                        | "ExposureTime"
                        | "FNumber"
                        | "ExposureProgram"
                        | "ISOSpeedRatings"
                        | "ShutterSpeedValue"
                        | "ApertureValue"
                        | "BrightnessValue"
                        | "ExposureBiasValue"
                        | "MaxApertureValue"
                        | "SubjectDistance"
                        | "MeteringMode"
                        | "LightSource"
                        | "Flash"
                        | "FocalLength"
                        | "LensMake"
                        | "LensModel"
                        | "LensSerialNumber"
                        | "BodySerialNumber"
                        | "OwnerName"
                )
        }
        "http://ns.adobe.com/photoshop/1.0/" => matches!(
            name,
            "AuthorsPosition"
                | "CaptionWriter"
                | "Category"
                | "City"
                | "Country"
                | "Credit"
                | "DateCreated"
                | "Headline"
                | "Instructions"
                | "Source"
                | "State"
                | "SupplementalCategories"
                | "TransmissionReference"
                | "Urgency"
                | "CountryCode"
                | "Location"
                | "LegacyIPTCDigest"
        ),
        "http://ns.adobe.com/lightroom/1.0/" => {
            matches!(name, "hierarchicalSubject" | "weightedFlatSubject")
        }
        _ => false,
    }
}
fn canonical_value(node: Node<'_, '_>, depth: usize) -> Option<String> {
    if depth > 64 {
        return None;
    }
    let mut attributes = node
        .attributes()
        .map(|a| {
            (
                a.namespace().unwrap_or("").to_owned(),
                a.name().to_owned(),
                a.value().trim().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    attributes.sort();
    let mut children = Vec::new();
    let mut text = String::new();
    for child in node.children() {
        if child.is_element() {
            children.push((
                child.tag_name().namespace().unwrap_or("").to_owned(),
                child.tag_name().name().to_owned(),
                canonical_value(child, depth + 1)?,
            ));
        } else if child.is_text() {
            text.push_str(child.text()?);
        }
    }
    if attributes.is_empty() && children.is_empty() {
        Some(format!("s:{}", text.trim()))
    } else {
        Some(format!(
            "x:{}",
            serde_json::to_string(&(attributes, children, text.trim())).ok()?
        ))
    }
}
pub(super) fn properties(bytes: &[u8]) -> Option<BTreeSet<XmpProperty>> {
    let xml = std::str::from_utf8(bytes).ok()?.trim_end_matches('\0');
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .ok()?;
    for node in document.descendants().filter(|node| node.is_element()) {
        if !matches!(
            node.tag_name().namespace(),
            Some(RDF) | Some("adobe:ns:meta/")
        ) && !inside_description(node)
        {
            return None;
        }
    }
    let mut result = BTreeSet::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name((RDF, "Description")) && !inside_description(*node))
    {
        let subject = serde_json::to_string(&(
            node.attribute((RDF, "about")).unwrap_or(""),
            node.attribute((RDF, "nodeID")).unwrap_or(""),
        ))
        .ok()?;
        for attribute in node.attributes() {
            let namespace = attribute.namespace().unwrap_or("");
            let name = attribute.name();
            if (namespace == RDF && matches!(name, "about" | "nodeID"))
                || descriptive(namespace, name)
            {
                continue;
            }
            result.insert(XmpProperty {
                subject: subject.clone(),
                namespace: namespace.into(),
                name: name.into(),
                value: format!("s:{}", attribute.value().trim()),
            });
        }
        for child in node.children().filter(|child| child.is_element()) {
            let namespace = child.tag_name().namespace().unwrap_or("");
            let name = child.tag_name().name();
            if descriptive(namespace, name) {
                continue;
            }
            result.insert(XmpProperty {
                subject: subject.clone(),
                namespace: namespace.into(),
                name: name.into(),
                value: canonical_value(child, 0)?,
            });
        }
    }
    Some(result)
}
