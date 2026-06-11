# Fayls

Fayls is a minimal self-hosted file browser - an alternative to `python -m http.server` or Apache's
directory index (mod_dir), but with search and sharing functionality.

<img width="1020" alt="Fayls browsing screenshot" src="https://github.com/user-attachments/assets/08ea9e94-48c1-47f5-8578-c9a3300a86e5" />

## Features

- Lists dirs and files
- Sort by name/last modified date/size
- Live server updates with sse
- Search by filename or contents
- Download files
- Preview some files:
    - images
    - pdf
    - docx
    - epubs
    - any utf8 encoded text files
    - common audio/video formats
    - (more on the way!)
- Sharing
    - create shared links to share files/folders.
    - shared link base domain configurable
    - shared views get the same functionality: (listing, downloading, previewing, searching), only
    scoped to the shared path

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

Default configuration is documented and loaded from `src/default_config.yaml`. Loaded configuration
is merged with the default one, so you don't have to copy all the entries just to overwrite a few.
