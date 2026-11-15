The adapter identity fields and endpoints are transcribed from official Immich v3.3.5 OpenAPI at commit `8303cb117e197c700332b33b61b45314c9cc5bb4`:
https://github.com/immich-app/immich/blob/8303cb117e197c700332b33b61b45314c9cc5bb4/open-api/immich-openapi-specs.json

`contract.json` contains only the relevant contract fields. `asset.json` is synthetic and includes every required AssetResponseDto field; its checksum is a synthetic base64 SHA1 value, never a BLAKE3 digest. No private response, credentials or photograph is included.

Favorite support also covers v3.4.0, whose [asset controller](https://github.com/immich-app/immich/blob/v3.4.0/server/src/controllers/asset.controller.ts) retains GET/PUT `/assets/:id` and whose [update schema](https://github.com/immich-app/immich/blob/v3.4.0/server/src/dtos/asset.dto.ts) accepts `isFavorite`. Tests cover both bare and v-prefixed server versions. Qualification records the exact server string, and evidence from a different version cannot authorize writes. This does not expand the supported star-rating versions.

Authenticated requests use the configured HTTP(S) origin, `/api`, and `x-api-key`. Redirects are disabled. Server version evidence is required separately before enabling writes. In this schema, logical rating zero serializes as null; numeric zero is not an accepted write. Missing EXIF observation and explicit unrated values remain distinct. Schema inspection and loopback HTTP tests do not establish deployed compatibility or compare-and-set guarantees.
