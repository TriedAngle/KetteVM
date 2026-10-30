#!/usr/bin/env node
// Issue-snippet conformance: every code snippet in the issues a PR
// references must behave the same under kettevm and node.
//
// Rules (issue snippets as live tests):
//   - Snippet fences: untagged, ```javascript or ```kette.
//   - First line matching /^NOT\s+A\s+TEST/ marks the snippet as
//     explanatory — skipped.
//   - Untagged/javascript: run kettevm and node, compare stdout.
//       * stdout differs                        -> FAIL
//       * kettevm exits nonzero, node zero      -> FAIL (kette crash)
//       * both exit nonzero with equal stdout   -> PASS (both threw)
//       * either engine times out               -> FAIL
//   - kette: node has no kette semantics, so there is no oracle —
//     kettevm must simply run it without crashing or timing out.
//
// Env: GITHUB_TOKEN, GITHUB_REPOSITORY, SNIPPET_PR_NUMBER, KETTEVM.
// Exit code 0 = all snippets pass (or nothing to check).

import { spawnSync } from "node:child_process";
import { allRefs, closingRefs, errorClass, isNotATest, snippetsFrom } from "./issue-snippets-lib.mjs";
import { mkdtempSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const API = "https://api.github.com";
const RUN_TIMEOUT_MS = 10_000;

const repo = required("GITHUB_REPOSITORY");
const token = process.env.GITHUB_TOKEN ?? "";
// Only needed when SNIPPET_ISSUE_NUMBERS is not set (the PR path).
if (!process.env.SNIPPET_ISSUE_NUMBERS) required("SNIPPET_PR_NUMBER");
const prNumber = process.env.SNIPPET_PR_NUMBER ?? "";
const kettevm = process.env.KETTEVM ?? "/target/release/kettevm";

function required(name) {
    const v = process.env[name];
    if (!v) {
        console.error(`missing env ${name}`);
        process.exit(2);
    }
    return v;
}

async function api(path) {
    const headers = {
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    };
    // The token is optional: the repo is public, so unauthenticated
    // reads work (lower rate limit) — handy for local runs.
    if (token) headers.Authorization = `Bearer ${token}`;
    const res = await fetch(`${API}${path}`, { headers });
    if (!res.ok) {
        throw new Error(`GET ${path} -> ${res.status} ${res.statusText}`);
    }
    return res.json();
}

// ── reference extraction ─────────────────────────────────────────

async function prText() {
    const pr = await api(`/repos/${repo}/pulls/${prNumber}`);
    const commits = await api(`/repos/${repo}/pulls/${prNumber}/commits`);
    const text = [
        pr.title ?? "",
        pr.body ?? "",
        ...commits.map((c) => c.commit?.message ?? ""),
    ].join("\n");
    return { text };
}

// ── markdown fence extraction ────────────────────────────────────


// ── engines ──────────────────────────────────────────────────────

function run(cmd, args, cwd) {
    const r = spawnSync(cmd, args, {
        cwd,
        encoding: "utf8",
        timeout: RUN_TIMEOUT_MS,
        killSignal: "SIGKILL",
        maxBuffer: 16 * 1024 * 1024,
    });
    return {
        status: r.status,
        signal: r.signal,
        stdout: r.stdout ?? "",
        stderr: r.stderr ?? "",
    };
}

// ── main ─────────────────────────────────────────────────────────

const failures = [];
const passes = [];
const skips = [];
const reported = [];
const warnings = [];

/// A GitHub Checks annotation (single-line: newlines are escaped).
function annotate(level, text) {
    const one = text.replace(/\r?\n/g, " \\n ");
    console.log(`::${level} ::${one}`);
}

/// Record a verdict: a failure message on an ENFORCED issue gates the
/// CI; on a merely-referenced issue it is report-only (master still
/// has that bug — the reference is context, not a claim).
function record(message, passId, warning) {
    if (message === undefined) message = null;
    if (passId === undefined) passId = null;
    const enforcedNow = currentIssueEnforced;
    if (warning) {
        warnings.push(`${passId ?? warning}: ${warning}`);
        annotate("warning", `${passId}: ${warning}`);
    }
    if (message === null) {
        if (enforcedNow) passes.push(passId);
        else reported.push(`PASS ${passId} (referenced, non-gating)`);
        return;
    }
    if (enforcedNow) {
        failures.push(message);
        annotate("error", message);
    } else {
        reported.push(`FAIL ${message} (referenced, non-gating)`);
        // A referenced mismatch is NOT a gate — but it stays visible
        // as a notice annotation on the check run.
        annotate("notice", message);
    }
}
let currentIssueEnforced = true;

// Local/debug path: SNIPPET_ISSUE_NUMBERS="12,34" checks those issues
// directly without a PR (used for smoke-testing the harness itself).
const direct = process.env.SNIPPET_ISSUE_NUMBERS
    ? process.env.SNIPPET_ISSUE_NUMBERS.split(",").map((s) => Number(s.trim())).filter(Boolean)
    : null;
// Closing refs ("fixes #N") vs plain references: a CLOSING ref means
// the fix is in THIS branch — its snippets must pass. A plain
// reference is context ("tracked in #N") — its snippets still run,
// but report-only, because master still has the bug by design.
let refs, enforced;
if (direct) {
    refs = direct;
    enforced = new Set(direct);
} else {
    const { text } = await prText();
    refs = allRefs(text);
    enforced = new Set(closingRefs(text));
}
if (refs.length === 0) {
    console.log("no issue references in the PR (title/body/commits) — nothing to check");
    process.exit(0);
}
console.log(`issue references: ${refs.map((n) => `#${n}`).join(" ")}`);

const work = mkdtempSync(join(tmpdir(), "issue-snippets-"));
let counter = 0;

for (const n of refs) {
    let issue;
    try {
        issue = await api(`/repos/${repo}/issues/${n}`);
    } catch (e) {
        console.log(`#${n}: cannot load (${e.message}) — skipped`);
        continue;
    }
    if (issue.pull_request) {
        console.log(`#${n}: is a pull request, not an issue — skipped`);
        continue;
    }
    const isEnforced = enforced.has(n);
    currentIssueEnforced = isEnforced;
    const body = `${issue.title ?? ""}\n\n${issue.body ?? ""}`;
    const snippets = snippetsFrom(body);
    console.log(
        `#${n}: ${isEnforced ? "FIXED by this PR — snippets must pass" : "referenced — snippets report-only"}`
    );
    if (snippets.length === 0) {
        console.log(`#${n}: no testable snippets`);
        continue;
    }

    snippets.forEach((s, i) => {
        const id = `#${n} snippet ${i + 1} (${s.lang || "untagged"})`;
        if (isNotATest(s)) {
            skips.push(id);
            return;
        }
        counter += 1;
        const ext = s.lang === "kette" ? "ktt" : "js";
        const file = join(work, `snippet-${counter}.${ext}`);
        writeFileSync(file, s.text + "\n");

        if (ext === "ktt") {
            const k = run(kettevm, [file], work);
            if (k.signal === "SIGKILL" || k.status === null) {
                record(`${id}: kettevm timed out (> ${RUN_TIMEOUT_MS}ms)`);
            } else if (k.status !== 0) {
                record(
                    `${id}: kettevm crashed (exit ${k.status})\n` +
                        `  kettevm stderr: ${k.stderr.trim().split("\n").slice(0, 5).join("\n")}`
                );
            } else {
                record(null, id);
            }
            return;
        }

        const node = run("node", [file], work);
        const kette = run(kettevm, [file], work);
        const label = (r) =>
            r.signal === "SIGKILL" || r.status === null
                ? "timed out"
                : `exit ${r.status}`;

        if (node.signal === "SIGKILL" || node.status === null) {
            record(`${id}: node timed out — no oracle for this snippet`);
            return;
        }
        if (kette.signal === "SIGKILL" || kette.status === null) {
            record(`${id}: kettevm timed out (node: ${label(node)})`);
            return;
        }
        // Syntax-error triage (review ask): a snippet that fails to
        // PARSE in both engines is a match (same verdict) — but it is
        // not a runnable test, so warn with the ignore recipe. A
        // syntax error on exactly ONE side is a mismatch no matter
        // what the stdout says.
        const kSyntax = kette.status !== 0 && errorClass(kette.stderr) === "SyntaxError";
        const nSyntax = node.status !== 0 && errorClass(node.stderr) === "SyntaxError";
        if (kSyntax && nSyntax) {
            record(
                null,
                id,
                "syntax error in BOTH engines — not a runnable test. To exclude " +
                    "it from the conformance run, make the snippet's first line " +
                    "`NOT A TEST` (e.g. `NOT A TEST: illustrative`)."
            );
            return;
        }
        if (kSyntax !== nSyntax) {
            record(
                `${id}: syntax error on one side only\n` +
                    `  node:    ${nSyntax ? "SyntaxError" : label(node)}\n` +
                    `  kettevm: ${kSyntax ? "SyntaxError (parse error)" : label(kette)}\n` +
                    `  node stderr:    ${node.stderr.trim().split("\n").slice(0, 3).join("\n")}\n` +
                    `  kettevm stderr: ${kette.stderr.trim().split("\n").slice(0, 3).join("\n")}`
            );
            return;
        }
        if (node.stdout !== kette.stdout) {
            record(
                `${id}: output differs\n` +
                    `  node stdout:    ${JSON.stringify(node.stdout)}\n` +
                    `  kettevm stdout: ${JSON.stringify(kette.stdout)}\n` +
                    `  kettevm stderr: ${kette.stderr.trim().split("\n").slice(0, 5).join("\n")}`
            );
            return;
        }
        if (kette.status !== 0 && node.status === 0) {
            record(
                `${id}: kettevm crashed (${label(kette)}) where node ran fine\n` +
                    `  kettevm stderr: ${kette.stderr.trim().split("\n").slice(0, 5).join("\n")}`
            );
            return;
        }
        if (kette.status !== 0 && node.status !== 0) {
            // Both threw: the EXCEPTION CLASS must agree too, not just
            // the stdout (equal-but-empty output with a TypeError on
            // one side and a RangeError on the other is a divergence).
            const kClass = errorClass(kette.stderr);
            const nClass = errorClass(node.stderr);
            if (kClass && nClass && kClass !== nClass) {
                record(
                    `${id}: both threw, but different exceptions\n` +
                        `  node:    ${nClass}\n` +
                        `  kettevm: ${kClass}\n` +
                        `  kettevm stderr: ${kette.stderr.trim().split("\n").slice(0, 5).join("\n")}`
                );
                return;
            }
            if ((kClass === null) !== (nClass === null)) {
                record(
                    `${id}: both threw, but only one side looks like a JS exception\n` +
                        `  node:    ${nClass ?? "(unrecognized)"}\n` +
                        `  kettevm: ${kClass ?? "(unrecognized)"}\n` +
                        `  kettevm stderr: ${kette.stderr.trim().split("\n").slice(0, 5).join("\n")}`
                );
                return;
            }
        }
        record(null, id);
    });
}

console.log("\n── summary ──────────────────────────────────────");
for (const p of passes) console.log(`PASS ${p}`);
for (const s of skips) console.log(`SKIP ${s} (NOT A TEST)`);
for (const r of reported) console.log(r);
for (const f of failures) console.log(`FAIL ${f}`);
console.log(
    `${passes.length} passed (enforced), ${reported.length} reported (referenced), ` +
        `${skips.length} skipped, ${failures.length} failed`
);

process.exit(failures.length === 0 ? 0 : 1);
