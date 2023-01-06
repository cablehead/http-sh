# Static File Example

Run with:

```bash
http-sh --static-path ./examples/static --port 3333 -- bash -c "echo {}; jq .path"
```

## sanitize.css.gz

CSS is provided by [`sanitize.css`](https://csstools.github.io/sanitize.css/).
Generated with:

```bash
curl -s -L https://github.com/csstools/sanitize.css/archive/refs/tags/v13.0.0.tar.gz | \
    tar xf - --to-stdout "*.css" | \
    minify --type=css | \
    gzip -c > css/sanitize.css.gz
```


