# http-sh

[![CI](
    https://github.com/cablehead/http-sh/actions/workflows/ci.yml/badge.svg)](
        https://github.com/cablehead/http-sh/actions/workflows/ci.yml)

## Overview

### GET: Hello world

```bash
$ http-sh :5000 -- echo Hello world
$ curl -s localhost:5000
Hello world
```

### POST: echo

```bash
$ http-sh :5000 -- cat
$ curl -s -d Hai localhost:5000
Hai
```

### Request metadata

The Request metadata is available as JSON on file descriptor 3.

Pairs well with [`jq`](https://github.com/stedolan/jq)

```bash
$ http-sh :5000 -- bash -c 'jq <&3'
$ curl -s localhost:5000
{
  "headers": {
    "accept": "*/*",
    "host": "localhost:5000",
    "user-agent": "curl/7.79.1"
  },
  "method": "GET",
  "path": "/",
  "proto": "HTTP/1.1",
  "query": {},
  "remote_ip": "127.0.0.1",
  "remote_port": 51435,
  "request_id": "0391ND23LWW4KVCZ00G30BZAG",
  "uri": "/"
}

$ http-sh :5000 -- bash -c 'echo hello: $(jq -r .path <&3)'
$ curl -s localhost:5000/yello
hello: /yello
```

### Response metadata

You can set the Response metadata by writing JSON on file descriptor 4.
Currently you can set the Response `status` and `headers`.

Pairs well with [`jo`](https://github.com/jpmens/jo)

```
$ http-sh :5000 -- bash -c 'jo status=404 >&4; echo sorry, eh'
$ curl -si localhost:5000
HTTP/1.1 404 Not Found
content-type: text/plain
transfer-encoding: chunked
date: Sat, 25 Feb 2023 05:20:48 GMT

sorry, eh
```

Note, for streaming responses, you'll want to close fd 4, so the Response is
initiated.

```
$ http-sh :5000 -- bash -c 'exec 4>&-; while true ; do date; sleep 1; done'
$ curl -s localhost:5000
Sat Feb 25 00:31:41 EST 2023
Sat Feb 25 00:31:43 EST 2023
Sat Feb 25 00:31:44 EST 2023
Sat Feb 25 00:31:45 EST 2023
Sat Feb 25 00:31:46 EST 2023
...
```

### [server-sent events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events)

Pairs well with [`xcat`](https://github.com/cablehead/xcat)

```bash
$ http-sh :5000 -- bash -c '
    jo headers="$(jo "content-type"="text/event-stream")" >&4
    exec 4>&-
    tail -F source.json | xcat -- bash -c "sed '\''s/^/data: /g'\''; echo;"
'

# simulate generating events in a seperate process
$ while true; do jo date="$(date)" ; sleep 1 ; done >> source.json

$ curl -si localhost:5000/
HTTP/1.1 200 OK
content-type: text/event-stream
transfer-encoding: chunked
date: Sat, 25 Feb 2023 18:13:37 GMT

data: {"date":"Sat Feb 25 13:13:35 EST 2023"}

data: {"date":"Sat Feb 25 13:13:36 EST 2023"}

data: {"date":"Sat Feb 25 13:13:37 EST 2023"}

data: {"date":"Sat Feb 25 13:13:38 EST 2023"}

data: {"date":"Sat Feb 25 13:13:39 EST 2023"}

...
```
