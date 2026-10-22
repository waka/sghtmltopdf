The PDF catalog gets a `/Lang` entry from the document's `<html lang>`, so viewers, screen
readers and PDF/A validators know the document language. A region tag such as `en-GB` is
kept as written, and an empty or missing `lang` writes no entry. Both the batch and the
streaming writer do this. Only the main document counts, not the header and footer HTML.
