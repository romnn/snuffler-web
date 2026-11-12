import assert from "node:assert/strict";
import { splitComputeBudget } from "../web/compute_budget.js";

for (const cores of [undefined, NaN, Infinity, -4, 0, 1, 2, 3, 4, 5, 6, 8, 9, 48, 192, 3.5]) {
  const budget = splitComputeBudget(cores);
  assert.equal(budget.rayonThreads + budget.openmpThreads, budget.total);
  assert.ok(budget.openmpThreads >= 1 && budget.openmpThreads <= 4);
  assert.ok(budget.rayonThreads >= 0 && budget.rayonThreads <= 4);
  assert.ok(budget.total <= 8);
  if (cores !== undefined && Number.isFinite(cores) && cores >= 1) {
    assert.ok(budget.total <= (cores > 2 ? Math.floor(cores) - 1 : Math.floor(cores)));
  }
}
assert.equal(splitComputeBudget(1).rayonThreads, 0);
assert.equal(splitComputeBudget(2).rayonThreads, 1);
assert.equal(splitComputeBudget(4).openmpThreads, 2);
assert.equal(splitComputeBudget(9).openmpThreads, 4);
console.log("all shared compute-budget checks passed");
