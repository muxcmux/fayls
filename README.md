# Fayls

Fayls is a minimal self-hosted file browser - an alternative to `python -m http.server` or Apache's
directory index (mod_dir), but with search and sharing functionality.

<img width="1020" alt="Fayls browsing screenshot" src="https://github.com/user-attachments/assets/08ea9e94-48c1-47f5-8578-c9a3300a86e5" />

## Features

- Lists dirs and files
- Preview some files - images, pdf (uses browser pdf viewer), docx, epubs, and utf8 text (more
  coming)
- Sort by name/last modified date/size
- Live server updates with sse
- Search by filename or contents - indexes text from pdf, office docs, ebooks, and utf8 readable
  files. Does ocr for images with tesseract (can be very slow if you run a potato, so remove the
  image extensions from the `indexing.index_contents_whitelist` default config key).
- Create shared links to share files/folders. You can configure the base domain of the shared links
  to point to a different domain with the `app.share_url` setting

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

Fayls only supports basic auth, which is useless if not behind tls terminated connection, so make
sure you run this behind caddy/nginx or somethig that handles certs and https.

*NOTE*: If you prefer not to store the admin creds in the yaml config file directly, you can pass
them from the environment. You can actually pass any config as env vars prefixed with `FAYLS_`, so
for the admin creds that would be `FAYLS_AUTH_USER` and `FAYLS_AUTH_PASS`

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
`/fayls/data`: this is where fayls looks for the default config and will create its database and
cache.

That's it. Now go to http://localhost:8080 and browse.

## Configuration

Default config file is in `src/default_config.yaml`. Image comes with the `fayls` server executable,
the `extractor` bin and `tesseract` bin, so no need to install anything additional. Tesseract uses
the English and Bulgarian lang packs. If you need to install more langs, you'd have to modify the
image and do an `apk add tesseract-ocr-data-*` where `*` is the lang you want. Rest of the entries
should be self-explanatory.
