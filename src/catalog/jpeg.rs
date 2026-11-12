use super::*;
use crate::domain::Digest;
use crate::import::{JPEG_FINGERPRINT_VERSION, JpegInspection};
use rusqlite::{OptionalExtension, params};
impl Catalog {
    pub fn cache_jpeg_inspection(&mut self, inspection: &JpegInspection) -> AppResult<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO blobs(digest,size) VALUES(?,?) ON CONFLICT DO NOTHING",
            params![inspection.digest.as_str(), inspection.size],
        )?;
        let size: u64 = tx.query_row(
            "SELECT size FROM blobs WHERE digest=?",
            [inspection.digest.as_str()],
            |row| row.get(0),
        )?;
        if size != inspection.size {
            return Err(AppError::integrity(
                "JPEG fingerprint size contradicts byte identity",
            ));
        }
        let previous: Option<Option<String>> = tx
            .query_row(
                "SELECT fingerprint FROM jpeg_fingerprints WHERE digest=? AND algorithm=?",
                params![inspection.digest.as_str(), JPEG_FINGERPRINT_VERSION],
                |row| row.get(0),
            )
            .optional()?;
        let fingerprint = inspection.image.as_ref().map(|value| value.as_str());
        if previous
            .as_ref()
            .is_some_and(|value| value.as_deref() != fingerprint)
        {
            return Err(AppError::integrity(
                "JPEG fingerprint contradicts cached byte identity",
            ));
        }
        tx.execute("INSERT INTO jpeg_fingerprints(digest,algorithm,fingerprint) VALUES(?,?,?) ON CONFLICT DO NOTHING", params![inspection.digest.as_str(), JPEG_FINGERPRINT_VERSION, fingerprint])?;
        tx.commit()?;
        Ok(())
    }
    pub fn jpeg_fingerprint(&self, digest: &Digest) -> AppResult<Option<Option<Digest>>> {
        let value: Option<Option<String>> = self
            .db
            .query_row(
                "SELECT fingerprint FROM jpeg_fingerprints WHERE digest=? AND algorithm=?",
                params![digest.as_str(), JPEG_FINGERPRINT_VERSION],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|value| value.map(|value| Digest::parse(&value)).transpose())
            .transpose()
    }
}
