# Product

## Register

product

## Users

ghinvite serves GitHub account admins who manage repository collaborator access and recipients who request access through invitation links. Account admins use the app while making access decisions for personal accounts or organizations. Recipients use the public invitation flow to understand what access they are requesting and confirm their GitHub identity.

## Product Purpose

ghinvite turns ad-hoc collaborator invitation requests into controlled invitation-link workflows. Success means admins can create links, review requests, and understand permission consequences without leaving the app unsure about repository access risk. Recipients should know what they are requesting and what happens next.

## Brand Personality

Precise, trustworthy, calm. The interface should feel like a polished Stripe-style console: structured, quiet, operational, and production-grade. Copy should be direct and specific rather than promotional.

## Anti-references

Avoid marketing-site presentation: oversized hero treatment, centered brochure copy, decorative sections, and excess whitespace that makes task screens feel like landing pages. Avoid generic SaaS card grids, decorative gradients, and visual novelty that competes with access decisions.

## Design Principles

- Make the app shell do more of the work: navigation, hierarchy, and spacing should make every screen feel like part of one console.
- Treat access decisions as operational work: permissions, repositories, approvals, and consequences should be visually clear before actions.
- Use polish through precision: alignment, rhythm, state styling, and component consistency matter more than decorative flourishes.
- Keep recipient flows focused: public pages should remain reassuring and task-oriented, not become marketing pages.
- Prefer familiar product affordances: standard navigation, form controls, buttons, tables, and status indicators should feel intentional and consistent.

## Accessibility & Inclusion

Use responsive layouts that remain usable on small screens, keyboard-friendly native links and buttons, readable contrast, and status treatment that does not rely on color alone. Keep motion minimal and purposeful for state feedback only.

## P1 changes from the proof of concept

- Invitation URLs carry an independent secret in the fragment. Creation and
  rotation show the URL once; lists and detail pages cannot reconstruct it.
  Internal link IDs no longer open public invitations. The manual code form
  remains available without JavaScript.
- Personal owners administer their own accounts. An organization's Installing
  Owner administers a repository-app installation; other repository admins need
  explicit, default-off delegation. Delegates see only links whose repositories
  they all administer, including scoped counts, request history and audit pages.
  Settings explain that the app may bypass GitHub's repository-admin invitation
  restriction before the owner enables delegation.
- Delegated repository discovery uses search. Selecting a repository checks
  access, and creation checks the complete selection again. An unavailable check
  preserves the form and offers recovery rather than treating uncertainty as
  permission. Installations above 5,000 repositories allow owner administration
  only; delegated authority is unavailable.
- Each selected repository has its own grant. Set all is a form-editing helper;
  changing that helper alone does not override individual grants or create a
  link. Approval and delivery retain the grants recorded at creation.
- Expiration defaults to 14 days. Never expires requires an explicit choice and
  warning. A link must retain an admission guardrail; revocation stops new
  requests without undoing existing requests or delivered access.
- Delivery distinguishes existing sufficient access, upgrades, preexisting
  invitations, blocked prerequisites, throttling and unknown outcomes. Unknown
  creates are reconciled without sending a second invitation. Policy and IP
  restrictions show specific guidance without exposing upstream error bodies.

These describe the implemented local behavior. They do not assert production
deployment or completion of M1 organization targets, lifecycle controls, budget
governance, or M2 bulk and email invitation workflows.
