# Bounded operational collection

This directory implements the local collection and configuration parts of P1
§14.2. It installs no hosted collector, notification channel or dashboard. The
operator must provide authenticated, complete captures and configure a private
Prometheus/textfile exporter. Missing evidence is **unavailable**, not zero health.

## Inputs and execution

Run Python 3.11 or later; the collector uses only the standard library:

```sh
python3 -m unittest discover -s deploy/observability -p 'test_*.py'
python3 deploy/observability/collector.py --input /private/p1-capture.json \
  --output /var/lib/p1-metrics/p1.prom
```

The input is an object with `generated_at` (UTC epoch seconds), plus the sources
below. Every capture must be at most five minutes old. The collector reads at
most 2 MiB, 10,000 records and 1,000 output series per source; duplicates, overflow,
missing required fields and future times make the affected source unavailable.
No raw input is logged. It atomically replaces its output on malformed input,
so a prior healthy snapshot cannot remain apparently current.

* `logs`: `{window_start, window_end, complete, records}`. The selected host
  export must cover the whole declared window (at most one hour), without loss,
  sampling, duplicate log delivery, pagination truncation or mixed clock origins. `log_sample.py`
  extracts the five existing/new event types from actual native/Worker tracing
  text, removing every field except the fixed allowlist. Example:
  `python3 deploy/observability/log_sample.py --input /private/host-window.log
  --window-start EPOCH --window-end EPOCH --complete --output /private/logs.json`.
  `--complete` is an operator assertion backed by the host export, not a
  completeness detector. Omit it when export coverage is uncertain. Worker log
  export wrappers must supply only their trusted text message field; arbitrary
  request payloads are never trusted log records. Time fields are emitted by
  the application, not guessed from collection time.
* `deliveries`: the **complete operator-authorized** `/console/operations.json`
  response. Existing web session, numeric operator authorization and no-store
  policy apply; no new machine credential or public metrics route is created.
  The collector ignores all per-step IDs and only exports bounded counts/ages.
  A report marked incomplete, or containing missing/future observation clocks,
  makes this source unavailable. Existing thresholds already account for each
  kind's observation cadence; the collector does not invent a universal cadence.
* `budgets`: `{observed_at, complete, snapshots: [BudgetSnapshot, ...]}` from
  authorized `InviteBudget` owner reads. Require complete account coverage. Fresh current closures and positive spam
  observations are exported immediately, including during the first day. The
  distinct `budget_limits` source requires positive 24-hour coverage before
  exporting full-window counts or zeroes. Old snapshots without limit counts,
  short coverage or future observations make that window unavailable; they do
  not suppress an independently fresh positive closure. Capturing an arbitrary
  subset must not be labelled complete. This is a private operator capture
  contract, not a new authoritative SQL read or a public scrape API.
* `journals`: output of `journal_sample.py --output /private/journals.json`,
  configured by `RESTATE_ADMIN_URL` and optional `RESTATE_ADMIN_TOKEN`. Only
  HTTPS or explicit loopback HTTP is accepted. Credentials never enter output;
  redirects are refused. The fixed admin query selects only completed
  `InvitationLink/admit` invocations and returns journal **counts only**. It has
  a three-second total read deadline and the same 2 MiB limit. A sentinel 101st
  row refuses a truncated sample; no rows, zero-length purged journals, network
  failure or an unexpected response shape are unavailable. The query is checked
  against pinned Restate 1.8.1, not presumed portable across admin schema changes.

Compose these source JSON objects into the envelope without changing their
capture times or completeness flags; failed sources can be omitted. Keep the
private input outside the textfile export directory. Only `p1.prom` is scraped.
The freshness alert also detects a stopped collector. Use private filesystem
permissions and private/loopback exporter binding; even numeric account and
installation dimensions are operational metadata, not public endpoints.

## Semantics and dashboards

`alerts.yml` and `dashboard.json` are importable templates; `prometheus.yml`
shows a local textfile scrape configuration. Window counts are **gauges**:
never use `rate()` or sum overlapping windows as event totals. p95 is exact for
that bounded sample; neither clocks nor independent windows are pooled. Remint
ratio is replacement mints after installation 401 divided by installation API
calls in the same window. With no denominator, the ratio is absent and its
`defined` metric is zero. Sustained signature alerts require windows of at most
60 seconds; wider windows explicitly report insufficient resolution instead of
treating one old failure as sustained new failures. App-JWT retries are a separate cause.

The requirement mapping is:

| §14.2 requirement | Actual source and presentation |
| --- | --- |
| OutcomeUnknown beyond one hour | Existing operator report `unknown.over_threshold`; alert plus age panel |
| Blocked reasons daily digest | Existing authenticated daily digest dispatch; bounded reason panel; no new email/channel product |
| Sent observation beyond twice cadence | Existing report `pending.over_threshold`; alert plus age panel |
| Budget closed/spammed | Fresh owner closure/positive restriction facts; complete-window counts separately |
| Mints and >1% remint ratio | Existing typed broker events, per app/numeric installation; five-minute sustained alert |
| Sustained signature rejection | Existing fixed signature events; five-minute alert |
| Binding rejection and auto-pause banner | Existing fixed edge events; panel. Authoritative link auto-pause remains the existing administrator Console banner/audit, not a sampled log inference |
| Admission latency and journal length vs SP16 | New attempt timing, retained count sample, optional proven baseline; no invented SP16 values |

Admission timing wraps the actual web ingress RPC and creates no Restate
journal steps, state writes or SQL writes. Exact replay produces another attempt
observation, not another approval. Cancellation reports `interrupted`; it does
not claim the authority had no effect. Abrupt process death may emit nothing.
Native elapsed time is monotonic. Worker host time is explicitly labelled,
checked for backwards/out-of-range values, and must not be compared against a
native baseline; a forward clock adjustment within the cap remains a host-clock
limitation. Timeout/error results use bounded classes, never errors, URLs,
request IDs, login/email, justification, signatures or capability values.

Production `InvitationLink` retains completed journals for only two seconds.
The count query is therefore an opportunistic **retained sample**, not all
admissions, and cannot prove a population p95 or the SP16 acceptance gate. The
local query fixture uses explicit longer retention solely to verify the query.
Do not change production retention to make this dashboard look healthy.

Optional `baseline` fields are `evidence_sha256`, `clock`,
`admission_latency_p95_seconds`, and `admit_journal_entries_p95`. Supply them only
from an actually recorded, same-clock SP16/comparable experiment. Without them,
`p1_baseline_available` is zero and comparison alerts have no values. The 20%
regression thresholds in the template are tunable operational policy, not a
claim that the experiment passed. Hosted rule installation, alert routing and
live production/SP16 observations remain separate gates.


Validate rule behavior with the pinned tool used by CI (no server is started):

```sh
docker run --rm --entrypoint /bin/promtool \
  -v "$PWD/deploy/observability:/config:ro" --workdir /config \
  prom/prometheus@sha256:6927e0919a144aa7616fd0137d4816816d42f6b816de3af269ab065250859a62 \
  test rules alerts-test.yml
```
