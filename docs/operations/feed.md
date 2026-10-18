# Feed operations runbook

How the feed vertical slice (tasks 12-FEED-01 through
12-FEED-03) works in production: a per-client-profile
activity timeline surfacing notable events from every
workstream, with pin / hide / read annotations. For the
auth model, see `security.md`; for the audit events the
feed emits, `audit-log.md`; for the feed security
posture, `threat-model.md`.

## What it is

Staff see one unified timeline per client profile:
contacts created or qualified, campaigns launched, forms
submitted, emails received, tasks due, notes added, agent
runs completed. Four layers:

- **Domain** (`crates/crm-domain/src/feed.rs`):
  `FeedItem` (write-once facts; `pin()`/`unpin()`/
  `hide()`/`mark_read()`), `FeedSource` (8 variants with
  stable `as_str()` names), `FeedVisibility`
  (`Normal`/`Pinned`/`Hidden` — `Hidden` is sticky, no
  `unhide()` on the domain), the pure
  `compare_feed_items()` comparator (pinned first, then
  newest-first, then `feed_item_id` — total and
  deterministic), `FeedQuery` (private fields; construct
  only via `FeedQuery::new(limit, include_hidden,
  unread_only)`; `limit` in `1..=MAX_FEED_LIMIT`
  (10,000); `Deserialize` is implemented manually
  through `new()` so the invariant holds for
  deserialized values too), and the `FeedService` /
  repository traits.
- **Persistence** (`crates/crm-db`, migration `0011_feed`):
  `feed_items` (append-mostly; only `visibility` and
  `read` change). SQL ordering mirrors the domain
  comparator (pinned → newest → id); the repository
  re-sorts with the pure comparator as a safety net.
- **Service** (`crates/crm-core/src/feed.rs`):
  `FeedServiceImpl` over the feed-item and audit ports.
  `list_feed` **requires a profile-scoped context**
  (fail-closed `Forbidden` otherwise); every operation
  records an audit event; audit failures are swallowed
  (fail-loud at the HTTP layer, never fail the
  operation).
- **HTTP** (`crates/crm-api/src/feed.rs`): 4 endpoints
  under `/api/v1` — feed list plus read/pin/hide
  annotations.

Every feed item belongs to one client profile — the
tenant boundary. Authorization uses the `FeedItem`
resource with profile-scoped grants (`feed_item/read`,
`feed_item/update`). A missing item is `404` before any
grant check (fetch-then-authorize).

## Feed model

Feed items are **write-once facts** produced by the other
workstreams. The feed never edits content: it orders,
filters, and annotates (read / pinned / hidden). There
is deliberately **no delete surface** — hidden items
stay in storage; they are only excluded from default
reads.

Each item carries a `ref_type`/`ref_id` pair linking back
to the source record (e.g. `ref_type: "contact"`,
`ref_id: <contact uuid>`), the `FeedSource` variant
naming the originating event, a non-blank `title`, an
optional `body`, and the `actor_kind` (human / agent /
system) of whoever caused the event.

## Ordering semantics

Reads return items **pinned first, then newest-first by
`created_at`**, with a `feed_item_id` tiebreak so the
order is total and deterministic even when two items
share a timestamp. The domain comparator
(`compare_feed_items`) is the source of truth; the
repository orders in SQL the same way and re-sorts with
the comparator as a safety net, and the service re-sorts
again before returning.

## Pin / hide / read lifecycle

```
Normal ⇄ Pinned      (pin / unpin, idempotent)
Normal/Pinned → Hidden   (hide, sticky — no unhide on the domain)
any → read = true    (mark_read, idempotent)
```

- `pin()` moves an item to the top band of reads;
  `unpin()` returns it to `Normal`. Both are idempotent
  and both **reject hidden items** — a hidden item must
  not reappear without an explicit service-level restore
  (a 12-FEED-02-era decision; the service trait does not
  expose restore, so hidden is effectively permanent
  through the current API).
- `hide()` is sticky and idempotent. Hidden items are
  excluded from reads unless the query passes
  `include_hidden: true`.
- `mark_read()` is idempotent. Read state is **shared**,
  not per-staff: one `read` flag per item for the whole
  profile.

Endpoints (`{id}` is the feed item UUID):

| Method & path | Grant | Effect |
|---|---|---|
| `GET /api/v1/feed?client_profile_id=<uuid>` | `feed_item/read` | List items for one profile, pinned-first then newest-first. Query params: `limit` (default 20, `1..=10000`, out-of-range → `422`), `include_hidden` (default false), `unread_only` (default false). Response: `{items, total}`. |
| `POST /api/v1/feed/{id}/read` | `feed_item/update` | Mark read (idempotent) |
| `POST /api/v1/feed/{id}/pin` | `feed_item/update` | Pin to the top band (idempotent; `422` on hidden items) |
| `POST /api/v1/feed/{id}/hide` | `feed_item/update` | Hide (sticky; idempotent) |

## Audit trail

The service records audit events with these pinned action
names (see `audit-log.md`):

`feed.listed`, `feed.item_read`, `feed.item_pinned`,
`feed.item_hidden`.

Audit is fail-loud at the HTTP layer and never fails the
operation: a recording failure is swallowed in the
service (loud warn) so feed work is never blocked by the
audit pipeline.

## Seed data

The dev seed (`crates/crm-db/examples/seed.rs`, gated by
`CRM_SEED_DEV=1`) creates four demo feed items on the
demo profile via `PgFeedItemRepository` (there is no
producer service yet, so the seed writes through the
repository directly): a pinned contact-created item, a
normal form-submitted item, a read email-received item,
and a hidden note — plus `feed_item` read/update grants
for the demo staff user:

```bash
CRM_SEED_DEV=1 DATABASE_URL=postgres://postgres:postgres@localhost:5432/crm_test \
    cargo run -p crm-db --example seed
```

The seed is idempotent (skipped when any feed items
already exist on the demo profile). Seeding uses the
repository — never raw SQL — so storage behavior matches
production.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `401` on feed endpoints | Missing bearer token | Re-login; the token lives in memory in the SPA. |
| `403` on `GET /api/v1/feed` | No `feed_item/read` grant on the profile | Have an operator check the `grants` table for `feed_item` on that profile scope. Grants are per-profile; cross-profile access is denied by design. |
| `403` on read/pin/hide | No `feed_item/update` grant | Annotations use the `Update` operation, not `Read`. |
| `403` on list with an agency context | `list_feed` requires a profile-scoped context | Pass `client_profile_id` in the query; the HTTP layer always does this, so this only triggers on misuse. |
| `422` on list | `limit` is 0 or above 10,000 | Keep `limit` in `1..=10000`. |
| `422` on pin | The item is hidden | Hidden items cannot be pinned; hidden is sticky. |
| `404` on read/pin/hide | Wrong id, or the item belongs to another profile | Fetch-then-authorize: a missing row is `404` before any grant check. |
| Empty feed | No producers yet | No workstream emits feed items automatically (see limitations); only the seed writes items. |

## Known limitations

- **No feed UI.** The Leptos frontend has no feed
  timeline, pin/hide controls, or unread badges — the
  feed is API-only until a frontend task picks it up.
- **No event producers yet.** No workstream service
  emits feed items automatically; the only writers are
  the repository (seed) and whatever future producer
  code ships. The 8 `FeedSource` variants are ready,
  but nothing publishes them today.
- **Read state is shared, not per-staff.** One `read`
  flag per item for the whole profile: any staffer
  marking an item read clears it for everyone. Per-user
  read records are a later task (with the schema and
  privacy questions that come with them).
- **Hidden is effectively permanent.** The domain has
  no `unhide()`, and the service trait exposes no
  restore operation — restoring a hidden item requires
  a future audited service decision.
- **`total` is the page length.** The list response's
  `total` counts the returned items, not the profile's
  full feed — there is no unfiltered count query yet.
