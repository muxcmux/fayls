# Fayls

Fayls is a minimal self-hosted file browser - an alternative to `python -m http.server` or Apache's
directory index (mod_dir), but with search functionality.

<img width="1020" alt="Fayls browsing screenshot" src="https://github.com/user-attachments/assets/08ea9e94-48c1-47f5-8578-c9a3300a86e5" />

## Features

- lists dirs and files
- preview some files - images, pdf (uses browser pdf viewer), docx, and utf8 text
- sort by name/last modified date/size
- live server updates with sse
- search by filename or contents - indexes text from pdf, office docs and utf8
  readable files (have to be whitelisted first though). does ocr for images
  with tesseract

## Run with Docker

Make a new dir somewhere and create a `config.yaml` file with the minimal config:

```yaml
app:
  auth:
    user: admin
    pass: 5up3r_h4xx0r
  sources:
    - /Documents
    - /some_dir
```

Add all the dirs you want to index/list under `sources`. Avoid adding subdirectories of already
added ones, e.g. `/Downloads`, and then `/Downloads/Games`.

Fayls only supports basic auth, which is useless if not behind tls terminated
connection, so make sure you run this behind caddy or somethig that handles
certs and https. Obviously choose a different user/pass combo.

If you fail to supply this minimal config, the container will not be able to start.

Since we are running with docker, `sources` here will just be paths the docker image sees.


```sh
$: docker run -p 8080:8080 \
              -u 1000:1000 \
              -v .:/fayls/data \
              -v /full/path/to/your/Documents:/Documents \
              -v /path/to/your/some_dir:/some_dir \
              ghcr.io/muxcmux/fayls:latest
```

Mount every corresponding entry in `sources` as a volume, and also the current directory to
`/fayls/data`: this is where fayls looks for the default config and will create its database
and cache.

That's it. Now go to http://localhost:8080 and browse.

## Configuration

Default config file is in `src/default_config.yaml`. Image comes with the `fayls` server executable,
the `extractor` bin and `tesseract` bin, so no need to install anything additional. Tesseract uses
the English and Bulgarian lang packs. If you need to install more langs, you'd have to modify the
image and do an `apk add tesseract-ocr-data-*` where `*` is the lang you want. Rest of the entries
should be self-explanatory.
