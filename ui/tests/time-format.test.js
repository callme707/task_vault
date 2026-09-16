"use strict";

// The UI renders stored ISO UTC values in the user's local timezone.
process.env.TZ = "Asia/Shanghai";
const assert = require("node:assert/strict");
const {
  formatDateTime,
  formatDateOnly,
  fmtDtLocal,
} = require("../main.js");

// 16:00Z is midnight on the next calendar day at UTC+08:00. This guards the
// exact boundary that used to be displayed as the previous local date.
assert.equal(formatDateTime("2024-03-01T16:00:00.000Z"), "2024-03-02 00:00");
assert.equal(formatDateOnly("2024-03-01T16:00:00.000Z"), "2024-03-02");
assert.equal(fmtDtLocal("2024-03-01T16:00:00.000Z"), "2024-03-02T00:00");

// The minute immediately before midnight must stay on the original day.
assert.equal(formatDateTime("2024-03-02T15:59:00.000Z"), "2024-03-02 23:59");
assert.equal(formatDateTime("not-a-date"), "");

console.log("time-format: ok");
