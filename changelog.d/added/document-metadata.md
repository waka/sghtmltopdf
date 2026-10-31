`--creator` and `--creation-date` set the PDF's `/Creator` and its creation date, and every PDF
now carries an XMP metadata stream. The creation date is an ISO 8601 date or date-time
(`2026-10-22T10:30:00+02:00`, converted to UTC) written as `/CreationDate` and `/ModDate` in
place of the current time; with it, the same input renders to the same bytes in both the batch
and the streaming writer. Without it the output is as before, apart from `/ModDate`, which is
now always written and equals the creation date. The XMP stream in the catalog's `/Metadata`
mirrors the Info fields (`dc:title`, `dc:creator`, `dc:description`, `pdf:Keywords`,
`xmp:CreatorTool`, `pdf:Producer`, `xmp:CreateDate`, `xmp:ModifyDate`) and the catalog's
`/Lang` (`dc:language`) as UTF-8; it carries no
PDF/A identification yet. Both options are available in the Rust API
(`DocumentMetadata::creator` and `creation_date`), the HTTP server's query and the Ruby
binding (`creator:`, and `creation_date:` taking a `Time`, `DateTime`, `Date` or ISO 8601 string).
