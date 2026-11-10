# AC-DLV-3 generated delivery sequences

These integration tests exercise the actual RepositoryDelivery and
OrganizationDelivery Restate owners, real native HTTP transport, SQLite attempt
fences/projections and the organization InviteBudget. They implement the literal
P1 §5.3 / build-plan B0-10 response-sequence acceptance; they do not establish
live GitHub classifications, production rollout or full milestone acceptance.

Each delivery kind defaults to24 deterministic seeds. The first physical write
of each seed rotates through201,204,401,403,404,422 validation/already-exists/
spammed/policy variants, a real HTTP header timeout, and primary/secondary rate
limits. Five further generated response symbols and a final201 form the suffix.
The test requires every alphabet member to have been consumed by an actual HTTP
write, and at least one case to consume multiple symbols. Generating an
unreachable response is insufficient coverage.

Each seed generates26 owner operations: create/replay/status calls, bounded
real-deadline waits and complete presence/absence/acceptance/expiry observations.
The suffix deliberately submits conflicting settlement evidence and replays the
original create command. Positive presence may resolve an uncertain create;
absence must not. Selected201 cases additionally lose three actual run
acknowledgements. Team5 is included in organization commands and independently
checked for immutable create/settlement and physical PUT counters.

The oracle uses a separately written terminal-outcome table rather than the
production `confirmed()` or transition reducers. It freezes the complete first
confirmed receipt and first settlement, checks physical mutation counters and
actual durable audit multiplicity per root/team target after every operation, and requires real
settlements. Organization cases also retain/check actual budget charges for
accepted or uncertain writes and exact observed spam/rate counts. Controls mutate
actual sampled receipts, settlements, counters and budget evidence; each required
control must execute before the aggregate gate passes. The Python verifier
independently regenerates plans and checks the complete recorded histories,
physical fence generations, provider traces, projected audit identities and
budget counters. All raw root/team facts are retained; one organization and one
team settlement are checked independently. Its self-test mutates actual runtime reports.

## Run and reproduce

Run from the repository root, with the normal integration environment and
an actual `RESTATE_SERVER_BIN` (or the supported owned runtime URLs):

```sh
GHINVITE_DELIVERY_PROPERTY_DIR=/absolute/fresh/evidence \
  cargo test --locked -p ghinvite-workflows --features integration \
  --test repository_protocol --test organization_delivery \
  ac_dlv_3_generated -- --test-threads=1
python3 crates/ghinvite-workflows/tests/common/verify-delivery-sequences.py \
  /absolute/fresh/evidence --self-test --output /absolute/fresh/verification.json
```

`GHINVITE_DELIVERY_PROPERTY_SEED=17` reproduces one seed for diagnosis.
`GHINVITE_DELIVERY_PROPERTY_CASES` permits12–96 full-run cases; the default24
covers each forced first symbol twice. The full two-kind Python acceptance gate
requires all response/control families, so a single-seed diagnostic run cannot be
misreported as complete acceptance. Use a fresh evidence directory for every run.

A seed reproduces response/action choices. Fresh runtime identifiers and wall
clock values remain real and are retained in each sample. `plan.json`,
`checkpoint.json` and `results.json` preserve the full generated plan and observed
prefix through the first failure, including actual owner snapshots, provider
calls, fences, budget data and settlement audits. This is not a claim of globally
minimal counterexamples. Passing runs additionally create per-kind summaries.

All new provider scripting and the500ms native read deadline are opt-in fixture
behavior. Existing fixed fixture scenarios keep their original handlers/deadlines.
The timeout handler actually stalls for3 seconds; the real client read deadline
produces uncertainty. Long spam/budget closures keep their original deadlines:
tests replay their refusal without rewriting clocks or waiting a day.
