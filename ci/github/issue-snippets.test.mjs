// Fence-parser unit tests (run: node ci/github/issue-snippets.test.mjs)
import { snippetsFrom, isNotATest, errorClass, allRefs, closingRefs } from "./issue-snippets-lib.mjs";

const cases = [
    {
        name: "js code + untagged code, rust block ignored entirely",
        body: ["```js", "a()", "```", "```rust", "let x;", "```", "```", "b()", "```"].join("\n"),
        want: [
            { lang: "js", first: "a()" },
            { lang: "", first: "b()" },
        ],
    },
    {
        name: "kette tag",
        body: ["```kette", "1 2 +", "```"].join("\n"),
        want: [{ lang: "kette", first: "1 2 +" }],
    },
    {
        name: "javascript tag still accepted",
        body: ["```javascript", "c()", "```"].join("\n"),
        want: [{ lang: "javascript", first: "c()" }],
    },
    {
        name: "info-string attributes: first word is the tag",
        body: ["```js title=x", "d()", "```", "```python title=y", "pass", "```"].join("\n"),
        want: [{ lang: "js", first: "d()" }],
    },
    {
        name: "crlf bodies",
        body: ["```js", "e()\r", "```\r"].join("\r\n"),
        want: [{ lang: "js", first: "e()" }],
    },
];

let failed = 0;
for (const c of cases) {
    const got = snippetsFrom(c.body).map((s) => ({
        lang: s.lang,
        first: (s.text.split("\n")[0] ?? ""),
    }));
    const ok =
        got.length === c.want.length &&
        got.every((g, i) => g.lang === c.want[i].lang && g.first === c.want[i].first);
    console.log(ok ? `PASS ${c.name}` : `FAIL ${c.name}\n  got:  ${JSON.stringify(got)}\n  want: ${JSON.stringify(c.want)}`);
    if (!ok) failed++;
}

const markerCases = [
    ["NOT A TEST: illustrative", true],
    ["NOT   A   TEST", true],
    ["not a test (lowercase does not match the spec regex)", false],
    ["console.log(1);", false],
];
for (const [line, want] of markerCases) {
    const got = isNotATest({ text: line });
    const ok = got === want;
    console.log(ok ? `PASS marker ${JSON.stringify(line)}` : `FAIL marker ${JSON.stringify(line)}: got ${got}, want ${want}`);
    if (!ok) failed++;
}

const classCases = [
    ["file:1\nTypeError: x is not a function", "TypeError"],
    ["Uncaught RangeError: bad", "RangeError"],
    ["uncaught exception: TypeError: nope", "TypeError"],
    ["parse error: expected `;`", "SyntaxError"],
    ["some plain stderr without a class", null],
    ["", null],
];
for (const [stderr, want] of classCases) {
    const got = errorClass(stderr);
    const ok = got === want;
    console.log(ok ? `PASS errorClass ${JSON.stringify(stderr.slice(0, 30))}` : `FAIL errorClass ${JSON.stringify(stderr)}: got ${got}, want ${want}`);
    if (!ok) failed++;
}

const refCases = [
    // [text, allRefs, closingRefs]
    ["Fixes #44 and refs #46", [44, 46], [44]],
    ["fix: #12", [12], [12]],
    ["this resolves TriedAngle/KetteVM#7", [7], [7]],
    ["Closes #1, tracking #2", [1, 2], [1]],
    ["mentioned #5 and see also #6", [5, 6], []],
    ["closed the loop on #9", [9], []], // keyword not adjacent to the ref
    ["RESOLVED: #31", [31], [31]],
    ["fixed (#40) — see #41", [40, 41], [40]],
];
for (const [text, wantAll, wantClosing] of refCases) {
    const a = allRefs(text);
    const c = closingRefs(text);
    const ok = JSON.stringify(a) === JSON.stringify(wantAll) && JSON.stringify(c) === JSON.stringify(wantClosing);
    console.log(ok ? `PASS refs ${JSON.stringify(text)}` : `FAIL refs ${JSON.stringify(text)}\n  all: ${a} want ${wantAll}\n  closing: ${c} want ${wantClosing}`);
    if (!ok) failed++;
}

process.exit(failed ? 1 : 0);
