# Rules of engagement — <CLIENT> / <ENGAGEMENT ID>

Fill this in before any scan. It is passed to Strix via `--instruction-file` and
pasted into the Cairn project as hints. Keep it short, explicit and negative —
what is *out* of scope matters more than what is in.

## Authorization

- Written authorization held: <reference>
- Window: <start> .. <end>
- Emergency contact / stop channel: <name, channel>

## In scope

- <hostname / URL / IP range> — <what kind of testing is permitted>

## Out of scope (never touch, not even to "check")

- Every other host, subdomain and IP range
- The CDN / WAF itself — navigate it, never attack it
- Third-party SaaS, auth providers, payment processors
- DoS / load testing of any kind

## Rate and safety limits

- Max ~5 requests/second; no fuzzing loops, no wordlist spraying
- No brute force / credential stuffing / anything that can lock out an account
- Read-only first: prefer GET/HEAD; send a body only when a read cannot answer
- No state change: no create/update/delete, no real business actions
  (no accounts, invoices, payments, messages, uploads to production)

## Stop and ask before

- Any write to the target
- Any action whose blast radius you cannot bound
- Anything that touches data belonging to another tenant
- Anything you would want a human to look at first

## Deliverable

- Every finding needs: what it is, where, how it was confirmed, and the exact
  request/reproduction. Unconfirmed = "unexamined", never "clean".
- Coverage gaps are reported as gaps, separately from findings.
