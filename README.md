# Fayls

Fayls is a minimal free open-source self-hosted file browser.

## Features

- lists dirs and files
- view some files - utf8 readable text, images, pdf
- sort by name/last modified date/size
- live server updates with sse
- search by filename or contents - indexes utf8 contents, text from pdf and does ocr for images with
  tesseract

## Run with Docker

Make a new dir somewhere and create a `config.yaml` file with the minimal config:

```yaml
app:
  sources:
    - /Documents
    - /some_dir
```

Add all the dirs you want to index/list under `sources`. Avoid adding subdirectories of already
added ones, e.g. `/Downloads`, and then `/Downloads/Games`.

Since we are running with docker, `sources` here will just be paths the docker image sees.


```sh
$: docker run -p 8080:8080 \
              -u 1000:1000 \
              -v .:/fayls/data
              -v /full/path/to/your/Documents:/Documents
              -v /path/to/your/some_dir:/some_dir
              fayls
```

Mount every corresponding entry in `sources` as a volume, and also the current directory to
`/fayls/data`: this is where fayls looks for the default config and will create its database.

That's it. Now go to http://localhost:8080 and browse.

## Configuration

Default config file is in `src/default_config.yaml`. Entries should be self-explanatory.
