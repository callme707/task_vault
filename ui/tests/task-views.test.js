"use strict";

const assert = require("node:assert/strict");
const {
  classifyTasks,
  mergeTaskSources,
  taskAfterAction,
  taskViewFor,
} = require("../main.js");

const task = (id, status, is_archived = false) => ({ id, status, is_archived });

const buckets = classifyTasks([
  task(1, "todo"),
  task(2, "in_progress"),
  task(3, "blocked"),
  task(4, "done"),
  task(5, "todo", true),
  task(6, "done", true),
]);

assert.deepEqual(buckets.active.map(({ id }) => id), [1, 2, 3]);
assert.deepEqual(buckets.completed.map(({ id }) => id), [4]);
assert.deepEqual(buckets.archived.map(({ id }) => id), [5, 6]);
assert.equal(taskViewFor(task(7, "done", true)), "archived");
assert.equal(taskViewFor(task(8, "unknown")), "active");
assert.deepEqual(
  classifyTasks(mergeTaskSources([task(14, "todo")], [task(14, "todo", true)])).archived.map(({ id }) => id),
  [14],
);

const completed = task(9, "in_progress");
const afterComplete = taskAfterAction(completed, "done");
assert.equal(completed.status, "in_progress");
assert.equal(taskViewFor(afterComplete), "completed");
assert.equal(taskViewFor(taskAfterAction(task(15, "todo"), "done")), "completed");

const active = task(10, "blocked");
assert.equal(taskViewFor(taskAfterAction(active, "archive")), "archived");
assert.equal(taskViewFor(taskAfterAction(afterComplete, "archive")), "archived");

const archivedTodo = task(11, "todo", true);
const restoredTodo = taskAfterAction(archivedTodo, "unarchive");
assert.equal(restoredTodo.status, "todo");
assert.equal(restoredTodo.is_archived, false);
assert.equal(taskViewFor(restoredTodo), "active");

const archivedDone = task(12, "done", true);
const restoredDone = taskAfterAction(archivedDone, "unarchive");
assert.equal(restoredDone.status, "done");
assert.equal(restoredDone.is_archived, false);
assert.equal(taskViewFor(restoredDone), "completed");

const reopened = taskAfterAction(task(13, "done"), "reopen");
assert.equal(reopened.status, "todo");
assert.equal(taskViewFor(reopened), "active");

console.log("task-views: ok");
