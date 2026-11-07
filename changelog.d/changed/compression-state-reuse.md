Streaming PDF output reuses one zlib compressor for page and decoration streams instead of
starting a fresh one per page. Compressed bytes are unchanged; documents with many pages
allocate substantially fewer bytes in total (peak live memory is unchanged).
