# SeqForge socket protocol

JSON-RPC 2.0 over a Unix domain socket. Used by the `seqforge` CLI and
by external agents to drive a running SeqForge GUI process. Stage 2.5d.

## Transport

- **Protocol**: JSON-RPC 2.0, newline-delimited (one request per line,
  one response per line).
- **Transport**: Unix domain socket. Path is published in the
  `SEQFORGE_SOCKET` environment variable (set in the SeqForge embedded
  terminal automatically; agents launched outside the GUI must read it
  from a wrapper or be told).
- **Path format**: `/tmp/seqforge-<pid>.sock`. One socket per GUI
  process; the path is removed when the GUI exits.
- **Concurrency**: each accepted connection runs in its own thread.
  Requests on a single connection are processed in order; the GUI's
  applier serializes everything to one mutation site so cross-connection
  request ordering is well-defined.

## Wire format

Request envelope:
```json
{"jsonrpc":"2.0","id":<any>,"method":"<method-name>","params":{...}}
```

Response envelope (success):
```json
{"jsonrpc":"2.0","id":<echoed>,"result":{"kind":"<variant>",...}}
```

Response envelope (error):
```json
{"jsonrpc":"2.0","id":<echoed>,"error":{"code":<int>,"message":"..."}}
```

`id` is echoed verbatim and supports any JSON value (numeric, string,
null). Notifications (no `id`) are **not** supported — every request
gets a reply.

## Methods

### `open`

Open a file in the viewer. Workspace-scoped: creates a new `View` and
its `Buffer` (or switches to an existing tab if the file is already
open by path). Replaces any `Tab::Welcome` placeholder.

```json
{"method":"open","params":{"path":"/abs/path/plasmid.gb"}}
```

Response: `{"kind":"ok"}`.

### `close`

Close the active view. Drops the underlying buffer if no other view
references it. ⌘W equivalent.

```json
{"method":"close","params":{}}
```

Response: `{"kind":"ok"}`. Errors with `NoActiveView` if nothing's open.

### `goto`

Navigate to a 1-based sequence position. Scrolls + places a cursor.

```json
{"method":"goto","params":{"position":1234}}
{"method":"goto","params":{"position":1234,"view":17}}    // explicit target
```

Response: `{"kind":"navigated","position":1234}`. Errors with
`OutOfRange` if `position` is 0 or exceeds the sequence length.

### `find`

IUPAC pattern search on both strands; the first hit becomes the
selection.

```json
{"method":"find","params":{"pattern":"GAATTC","mismatches":0}}
{"method":"find","params":{"pattern":"GAATTC","view":17}}    // explicit target
```

Response: `{"kind":"search_results","count":N,"hits":[{...}]}`.
Empty `pattern` clears results.

### `enzymes`

Show restriction sites for the named enzymes. Names matched
case-insensitively against the bundled enzyme library; unknown names
are silently skipped.

```json
{"method":"enzymes","params":{"enzymes":["EcoRI","BamHI"]}}
{"method":"enzymes","params":{"enzymes":["EcoRI"],"view":17}}    // explicit target
```

Response: `{"kind":"cut_sites","count":N,"sites":[{...}]}`. Empty
`enzymes` clears the cut-site overlay.

## View targeting

View-scoped methods (`goto`, `find`, `enzymes`) accept an optional
`view: <ViewId>` parameter:

- **Omitted (default)**: operates on the workspace's currently active
  view (`workspace.active_view`). Equivalent to clicking that tab and
  invoking the action by hotkey.
- **Provided**: operates on the view with that id explicitly. Returns
  `ViewNotFound` (error code `-32000`, message `view ViewId(N) not
  found`) if the view has been closed since the agent enumerated it.

Agents that operate across multiple open files should:
1. Track view ids returned from prior interactions (or extracted from
   future enumeration RPCs — not yet exposed).
2. Pass `view: <id>` explicitly to avoid races against user tab switches.
3. Be prepared to handle `ViewNotFound` and re-enumerate.

There is **no pane targeting**. After Stage 2.5c/e, panes are a layout
concept owned by `egui_dock` (split-view tab groups in the dock tree),
not addressable identity. The set of open views is the source of truth
for "what files are open"; how the user has arranged them spatially is
not part of the protocol surface.

## Error codes

Standard JSON-RPC codes plus one app-specific:

| Code     | Source     | Meaning                                        |
|----------|------------|------------------------------------------------|
| `-32700` | Parse      | Body wasn't valid JSON.                        |
| `-32600` | Invalid    | Couldn't deliver to the running viewer.        |
| `-32601` | Method     | Unknown method name.                           |
| `-32602` | Params     | Params didn't deserialize into the variant.    |
| `-32000` | App        | `DispatchError` from `seqforge_core::dispatch`.|

`DispatchError` variants surfaced under `-32000`:

- `NoActiveView` — request needs a view, none is active.
- `ViewNotFound(ViewId(N))` — explicit `view` target doesn't exist.
- `OutOfRange { position, seq_len }` — for `goto`.
- `PoisonedLock` — buffer's `RwLock` poisoned (panic in a previous
  writer; should never happen in single-threaded apply path).
- `BioError(msg)` — load / search / cut-site computation failed.
- `Unimplemented(name)` — placeholder for future variants.

## Timeouts

Each socket connection waits up to **5 seconds** for the GUI's applier
to process a request. If the GUI is busy beyond that (heavy paint,
modal dialog), the client gets:

```json
{"error":{"code":-32000,"message":"viewer did not respond within timeout"}}
```

The request may still complete inside the GUI; clients should not
retry a non-idempotent request after a timeout.

## Threat model

**The socket is a local control plane, not a network endpoint.**

- The socket path is in `/tmp/seqforge-<pid>.sock`. On a multi-user
  Unix host, anyone with read access to `/tmp` can see the path; access
  is gated by filesystem permissions on the socket file itself, which
  defaults to the owner's umask (typically `srwxr-xr-x`, so write
  access is owner-only on a normal setup).
- A connecting process is implicitly trusted: it can `open` arbitrary
  files (subject to GUI process's filesystem access), trigger
  arbitrary searches, and read sequence data. Any process running as
  the same user can do this.
- The protocol **does not authenticate**. There is no shared secret,
  no capability handshake. Adding one would block agent
  interoperability for the MVP and offers little real protection on a
  single-user dev host (the attacker can ptrace the GUI anyway).
- The socket exposes **no shell escape**: methods take typed params
  parsed via serde; method dispatch is a closed-enum match in Rust.
  Adversarial JSON cannot reach arbitrary file paths, exec syscalls,
  or untyped fields.
- `Open { path }` does **not** validate `path`. A malicious agent
  could load a 10 GB file and hang the GUI. This is a denial-of-service
  surface, not a privilege-escalation one — the attacker already had
  read access to the file.

**If/when SeqForge ships a multi-user or networked variant**, this
threat model needs revisiting: capability tokens, per-method
allow/deny, sandboxed buffer loaders, rate limiting on `find` /
`enzymes` for sequences over some threshold.

## CLI usage

The bundled `seqforge` CLI is the canonical client.

```bash
$ seqforge open /path/to/plasmid.gb
$ seqforge goto 1234
$ seqforge find GAATTC
$ seqforge find GAATTC --mismatches 1
$ seqforge enzymes EcoRI BamHI
$ seqforge close
```

View targeting:
```bash
$ seqforge goto 1234 --view 17
$ seqforge find GAATTC --view 17
```

File commands (`info`, `digest`, `annotate`) run locally without a
socket — they read sequence files directly from disk.
