# ADR 045 — An asset is its own hash, and a snapshot does not go live without its bytes

Status: accepted (milestone 13B)
Extends ADR 012 and ADR 044. Changes no decision taken in either.

## Context

Milestone 13A gave the catalog `AssetRef { content_hash, media_type, byte_size }`
and put it in the snapshot's content hash, deliberately without the bytes. A
kiosk is operated by a customer, so the bytes are not decoration: a menu with
no photographs is not a menu. And a till is on a shop floor, where the line
goes down — so fetching them when the screen draws is not an option either.

The failure to design against is specific and unacceptable:

```text
snapshot ACTIVE  +  required image missing  =  a kiosk showing holes
```

That must not be a state the system can reach, by any path, including a crash
in the middle of putting a catalog in force.

## Decision — the hash is the identity, and the filesystem is the state

```text
<runtime>/assets/staging/<hex>.part      bytes still arriving; never served
<runtime>/assets/sha256/<ab>/<hex>       verified bytes; immutable, shareable
```

An asset is identified by the SHA-256 of its bytes and by nothing else. Not
the filename it had upstream, not the URL it came from, not the product that
references it. Two snapshots showing the same photo share one file; importing
it again costs nothing and stores nothing.

**There is no asset table.** The content store *is* the state: a file at the
path its own digest dictates, of the declared length. This is not an
optimisation, it is the crash-recovery design — a process that promoted an
asset and then died before "recording" it has nothing left to record, so the
next attempt finds the file and converges. Every table that mirrors a
filesystem is a pair of facts that can disagree, and this one would have had
to be reconciled at every startup.

`sha256/<first two hex characters>/<full digest>` keeps directories small
without inventing a second naming scheme. The only input that ever becomes a
path component is the 64-character lowercase hex digest, **re-validated in the
function that builds the path** even though `AssetRef` validated it on the way
in. No client-supplied name, label or location reaches the filesystem, so
there is no string to traverse with.

## Decision — stage, verify, promote

The catalog's own lifecycle, applied to bytes:

| Step | What happens | What a reader can see |
| --- | --- | --- |
| `PutAssetChunk` ×N | bytes appended to `staging/<hex>.part` | nothing |
| `CommitAsset` | length, SHA-256 and media type checked, `fsync`, then one `rename` | the whole verified file, or nothing |

Promotion is a rename inside one filesystem, so there is no instant in which
half an asset is readable. A crash before the rename leaves a `.part` file,
which is resumable and never served; a crash after it leaves a verified asset,
which is the outcome anyway.

Offsets are strict: a chunk must start exactly where the staged file ends, so
no gap can ever be written. A chunk that re-sends bytes already on disk is a
**no-op**, because a transport that retries after a timeout must not be
punished for retrying — and the digest at commit is the authority on whether
those bytes were right in the first place.

When a commit proves the staged bytes wrong — bad digest, overlong, or not the
declared media type — the staged file is discarded, so the import can start
again cleanly rather than being stuck on a corpse. **The content store is never
touched by a failed staging.** A bad import must not cost an installation an
asset it already has.

## Decision — the client never says an asset is valid

It says what it is sending: a hash, a length, a media type. PI.Edge reads the
bytes it actually received, hashes them, and decides. There is no field
anywhere in the protocol by which a client could declare an asset verified,
and no path by which it could name a location.

Media type is checked twice, cheaply: an **allowlist** of what this runtime
holds at all, and a signature check for the formats where one is unambiguous
(PNG, JPEG, GIF, WebP). No decoding, no resizing, no transcoding, no
thumbnails — the stored bytes hash to exactly what the catalog declared,
because they are exactly what it declared. A container we cannot cheaply
recognise is accepted on its declaration, which is stated rather than implied.

## Decision — the manifest is derived, never maintained

`required_assets(snapshot_id)` is a query over the snapshot's own content rows:
category images, product presentation images and selection option images, as
distinct `(content_hash, media_type, byte_size)` triples in canonical order. A
second list that had to be kept in step with the content would eventually
disagree with it, and the disagreement would be invisible.

One consequence is a new rule the commit enforces: a snapshot may not declare
the same hash with two different lengths or media types. Content-addressed
identity allows exactly one answer per hash, and a snapshot that gives two has
an ambiguous manifest. This is the one place milestone 13A gained a check
(`CATALOG_CONFLICTING_ASSET_DECLARATION`), because 13B made the ambiguity
reachable.

## Decision — activation is the gate

`ActivateCatalogSnapshot` verifies every required asset **before anything is
dismounted**, inside the same transaction that activates, so there is no
instant between "the assets are here" and "this is the catalog".

Two passes, deliberately:

1. the cheap one — present, and of the declared length — so a snapshot whose
   images plainly have not arrived is refused without hashing anything;
2. the real one — **SHA-256 over the stored bytes** — because "a file of the
   right length exists" is not the promise the snapshot made.

A failure is one stable code, `CATALOG_ASSET_NOT_READY`, so a client reacts by
fetching the snapshot's manifest rather than by parsing a message. The snapshot
stays `PENDING` and resumable, and **the catalog that was selling keeps
selling**. The second pass is why content tampered with underneath the store —
same length, different bytes — stops the next activation instead of reaching a
kiosk.

A snapshot that references no asset never touches the store at all, which is
what leaves every retail catalog exactly as it was.

## Decision — present is required, absent is not missing

```text
AssetRef present  -> the bytes are obligatory and verifiable
AssetRef absent   -> this product has no photo
```

These are different facts and are never conflated. A product with no
presentation image blocks nothing; a product with one blocks activation until
the bytes are here. A local placeholder is a client's decision about an absent
reference, never a substitute for a reference that exists.

And there is **no fallback to a remote URL**, anywhere, for any reason. A POS
that quietly reaches the internet to draw a menu is a POS that stops drawing
it when the line goes down, which is the failure this whole milestone exists
to prevent.

## Decision — no transport, and no downloader

`LocalAssetStore` is handed bytes. Whether they came from a cloud sync, a
bundle, a USB stick, a local server or a test is not its concern, and
`edge-assets` contains no HTTP, no URL and no retry policy. There is no
`AssetSource` trait either: a trait with no implementation constrains nothing
and only looks like it does (see `CLAUDE.md`). The separation that matters is
already there — the store's API is chunks in, bytes out.

The CLI's `import-asset` is the development stand-in for all of those, exactly
as `import-catalog` stands in for the server: it hashes a local file, streams
it, and asks the core to verify it.

## Decision — nothing is ever evicted

Usage is measurable (`stored_bytes`) and a per-asset ceiling is configurable
(`PI_EDGE_MAX_ASSET_BYTES`, default 32 MiB, refused before a byte is written).
A full disk is its own explicit error, `ASSET_STORE_FULL`.

There is no garbage collection in this milestone, and an unreferenced asset
simply stays. That is the right bias: the store can already answer which
assets a snapshot needs, so a future collector has what it needs, and keeping
a file nobody references costs disk while deleting one somebody references
costs a kiosk. Staging cleanup is limited to `.part` files proven incomplete
or wrong; nothing in the content store is ever deleted without proof that
nothing references it, which this milestone does not attempt.

## Consequences

* A store that could not be created leaves the runtime with
  `UnconfiguredAssetStore`: every asset reads as `MISSING`, every write
  answers `ASSET_STORE_NOT_CONFIGURED`, and a snapshot that needs an image
  cannot be activated. The till still sells. That is honest, and it is never a
  silent fallback to a temporary directory or to the network.
* Activation now does file I/O inside a write transaction — one streamed pass
  over the snapshot's images. Bounded by the menu's artwork and paid once per
  activation, and the alternative is a window in which the assets were there a
  moment ago.
* Bytes cross the IPC frame as lowercase hex, which doubles them. Base64 would
  cost a third less and add a dependency; the digest is computed over the
  decoded bytes either way, so the encoding can change later without touching
  the asset model.
* Nothing here draws anything. No kiosk UI, no screensaver, no React
  integration, no CDN. The core can now answer "what does this snapshot need",
  "do I have it" and "give me the bytes" — offline — and that is the whole of
  the milestone.
