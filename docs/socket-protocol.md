# SeqForge socket protocol

JSON-RPC 2.0 over a local socket. Used by the `seqforge` CLI and
by external agents to drive a running SeqForge GUI process.

## Transport

- **Protocol**: JSON-RPC 2.0, newline-delimited (one request per line,
  one response per line).
- **Transport**: a local socket. On macOS and Linux this is a Unix
  domain socket; on Windows it is a named pipe. The endpoint is
  published in the `SEQFORGE_SOCKET` environment variable (set in the
  SeqForge embedded terminal automatically; agents launched outside
  the GUI must read it from a wrapper or be told).
- **Endpoint format**:
  - Unix: `$XDG_RUNTIME_DIR/seqforge-<pid>.sock` when that directory
    is set, otherwise `/tmp/seqforge-<pid>.sock`. The socket file is
    created mode `0600` and removed when the GUI exits.
  - Windows: `\\.\pipe\seqforge-<pid>` (access gated by the creating
    user's named-pipe ACL).
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

## Document targeting

Document-scoped methods carry a flattened **target**: two optional,
mutually exclusive params, `view` and `path`. They are flattened into the
params object, so they sit alongside the method's own fields rather than
nested.

| params | meaning |
|---|---|
| neither | the workspace's currently active view (`workspace.active_view`) |
| `"view": <ViewId>` | that view explicitly |
| `"path": "<file>"` | open that file and operate on it |

```json
{"method":"find","params":{"pattern":"GAATTC"}}                  // active view
{"method":"find","params":{"pattern":"GAATTC","view":17}}        // explicit view
{"method":"find","params":{"pattern":"GAATTC","path":"p.gb"}}    // a file
```

An omitted target serializes to nothing at all, so a request written
against the pre-target protocol parses unchanged.

- **`view` on a closed view** returns `ViewNotFound` (`-32000`, message
  `view ViewId(N) not found`).
- **`view` and `path` together** is rejected. The CLI's argument parser
  catches it first (`--in` conflicts with `--view`); a caller that speaks
  JSON-RPC directly gets a `DispatchError` from the target resolver.
- **`path` over the socket is refused.** `--in` means *headless*: it resolves in
  the calling process against a workspace that lives for one request, which is
  what the CLI already does (a path target never reaches the socket). A `path`
  arriving at a running session is therefore a category error — it asks the
  session to act on a document it does not have. `open` the file first, then
  target the resulting view.
- **`path` on a write method** returns `Unimplemented` — writing through a
  file target is not yet supported, and the file is left untouched.

### One rule for GUI updates

A request changes what is on screen **exactly when its target resolves to a live
view** — the active one, or the `view` named. That is a property of the target,
not of the verb. A `path` target never has a view, so it never updates anything;
that is the whole of the rule.

Which document a request names is part of what the request *means*, not
transport configuration — unlike `SEQFORGE_SOCKET`, which only decides
where the request is delivered. That is why it lives in `params`.

### Id provenance

`FeatureId` and `PrimerId` are **session-scoped** (decision 12). Ids
returned by a `path`-targeted request name entries in a workspace that
exists only for that one resolution, so they cannot address a document in
the running session — and vice versa. An agent that reads with `path` and
then writes with `view` must re-read ids from the session it is writing
to. This is inherent to ids being minted at load, not a gap.

Agents that operate across multiple open files should:
1. Track view ids returned from prior interactions, or enumerate with
   `buffers`.
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

- On Unix the endpoint is a filesystem socket under
  `$XDG_RUNTIME_DIR` or `/tmp`, created mode `0600`. On a multi-user
  host anyone who can see the path still cannot connect unless they
  can write the socket file. On Windows the endpoint is a named pipe
  whose default ACL is the creating user.
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

Document targeting:
```bash
$ seqforge goto 1234 --view 17          # a tab in the running viewer
$ seqforge find GAATTC --view 17
$ seqforge find GAATTC --in plasmid.gb  # a file — no socket, no viewer
```

Routing follows the document. A request naming a `path` runs in the
calling process against an ephemeral workspace; anything else is
forwarded over the socket. So `info`, `digest`, `orfs`, `translate`,
`primers`, and any read verb given `--in` need no viewer running, while
the same verb without a target requires one.
