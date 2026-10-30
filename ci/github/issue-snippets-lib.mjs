// Pure pieces of the issue-snippet harness, extracted for testing.

export const LANGS = new Set(["", "js", "javascript", "kette"]);
// `js` and `javascript` are the same language; the repo's issues tag
// code with ```js.
export const LANG_JS_ALIASES = new Set(["js", "javascript", ""]);

export const NOT_A_TEST = /^NOT\s+A\s+TEST/;

/// GitHub's issue-closing keywords. A reference in this form makes
/// GitHub AUTO-CLOSE the issue when the PR merges — and means the fix
/// is in THIS branch, so the issue's snippets must pass on it.
/// Accepted forms: `fixes #44`, `fix: #44`, `closes #44`,
/// `resolves owner/repo#44`, `fixed <issue-url>` (case-insensitive).
const CLOSING_REF =
    /\b(?:close|closes|closed|fix|fixes|fixed|resolve|resolves|resolved)\s*:?,?\s*(?:\()?\s*(?:https?:\/\/github\.com\/[\w.-]+\/[\w.-]+\/issues\/|(?:[\w.-]+\/[\w.-]+)?#)(\d+)\b/gi;

/// All plain issue references (`#N` anywhere).
export function allRefs(text) {
    const refs = [];
    for (const m of text.matchAll(/#(\d+)/g)) {
        const n = Number(m[1]);
        if (!refs.includes(n)) refs.push(n);
    }
    return refs;
}

/// The issues this text CLAIMS to fix (closing keywords only).
export function closingRefs(text) {
    const refs = [];
    for (const m of text.matchAll(CLOSING_REF)) {
        const n = Number(m[1]);
        if (!refs.includes(n)) refs.push(n);
    }
    return refs;
}

export function firstLine(text) {
    return text.split("\n")[0] ?? "";
}

export function isNotATest(snippet) {
    return NOT_A_TEST.test(firstLine(snippet.text));
}

export function snippetsFrom(body) {
    const out = [];
    const lines = body.split(/\r?\n/);
    // Tri-state: null = outside any fence, "ignore" = inside a fence
    // we do not collect (e.g. ```rust — its CLOSING fence must not be
    // mistaken for an untagged opening fence), otherwise the tag.
    let state = null;
    let buf = [];
    for (const line of lines) {
        if (state === null) {
            const m = line.match(/^```(.*)$/);
            if (m) {
                const info = m[1].trim();
                const tag = info.split(/\s+/)[0] ?? "";
                state = LANGS.has(tag) ? tag : "ignore";
                buf = [];
            }
        } else if (/^```/.test(line)) {
            if (state !== "ignore") out.push({ lang: state, text: buf.join("\n") });
            state = null;
        } else if (state !== "ignore") {
            buf.push(line.replace(/\r$/, ""));
        }
    }
    return out;
}

/// The exception class name from an engine's stderr, for the
/// both-threw comparison — or null when none is recognizable.
///   node:     "Uncaught TypeError: x is not a function"  -> TypeError
///   kettevm:  "uncaught exception: TypeError: ..."       -> TypeError
///   kettevm:  "parse error: expected ..."                -> SyntaxError
///             (kettevm's parse error is node's SyntaxError class)
export function errorClass(stderr) {
    if (/^\s*parse error/i.test(stderr)) return "SyntaxError";
    const m = stderr.match(/(?:uncaught exception:\s*|uncaught\s+)?\b([A-Z][A-Za-z0-9]*(?:Error))\b/);
    return m ? m[1] : null;
}
