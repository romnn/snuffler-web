// Transition-table checks for the request-local execution control: the atomic
// state machine that linearizes cancellation, interpreter ownership, and
// commit. Pure and synchronous, so it runs under plain bun with no worker —
// each check drives the exported API from a chosen stable state and asserts
// the resulting state and interrupt byte. (`Atomics.wait` paths are exercised
// only from states that never wait, since this runs single-threaded.)
import {
  ExecutionState,
  beginCommit,
  cancellationRequested,
  createExecutionControl,
  executionControl,
  markExecutionDone,
  requestCancellation,
  setInterpreterOwner,
} from "../web/execution_control.js";

// Mirrors the module-private ControlState encoding; a drift here fails loudly
// below because every assertion goes through the public API's behavior.
const COMPUTING_SUSPENDED = 1;
const COMPUTING_OWNED = 2;
const CANCELLING_SUSPENDED = 4;
const CANCELLING_OWNED = 5;
const COMMITTING = 6;
const DONE = 7;

let failures = 0;

function check(name: string, actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    failures += 1;
    console.error(`FAIL ${name}: expected ${String(expected)}, got ${String(actual)}`);
  } else {
    console.log(`ok   ${name}`);
  }
}

function fresh(state?: number): { control: Int32Array; interrupt: Uint8Array } {
  const control = createExecutionControl();
  if (state !== undefined) Atomics.store(control, 0, state);
  const interrupt = new Uint8Array(new SharedArrayBuffer(1));
  return { control, interrupt };
}

{
  // A new control is suspended: the worker grants ownership explicitly.
  const { control } = fresh();
  check("initial state is ComputingSuspended", Atomics.load(control, 0), COMPUTING_SUSPENDED);
}

{
  // executionControl re-wraps the same buffer, not a copy.
  const { control } = fresh(COMPUTING_OWNED);
  const rewrapped = executionControl(control.buffer as SharedArrayBuffer);
  check("executionControl shares the buffer", Atomics.load(rewrapped, 0), COMPUTING_OWNED);
}

{
  // Cancelling a suspended job must not write the interrupt byte: the
  // interpreter may be running another lane's Python.
  const { control, interrupt } = fresh(COMPUTING_SUSPENDED);
  const seen = requestCancellation(control, interrupt);
  check("suspended cancel reports Computing", seen, ExecutionState.Computing);
  check("suspended cancel -> CancellingSuspended", Atomics.load(control, 0), CANCELLING_SUSPENDED);
  check("suspended cancel leaves the byte", Atomics.load(interrupt, 0), 0);
}

{
  // Cancelling an owning job publishes the byte and lands in CancellingOwned.
  const { control, interrupt } = fresh(COMPUTING_OWNED);
  const seen = requestCancellation(control, interrupt);
  check("owned cancel reports Computing", seen, ExecutionState.Computing);
  check("owned cancel -> CancellingOwned", Atomics.load(control, 0), CANCELLING_OWNED);
  check("owned cancel arms the byte", Atomics.load(interrupt, 0), 2);
  check("cancellationRequested sees it", cancellationRequested(control), true);
  const again = requestCancellation(control, interrupt);
  check("second cancel reports Cancelling", again, ExecutionState.Cancelling);
}

{
  // Commit wins from ComputingOwned and clears the byte.
  const { control, interrupt } = fresh(COMPUTING_OWNED);
  Atomics.store(interrupt, 0, 2);
  check("beginCommit wins from owned", beginCommit(control, interrupt), true);
  check("owned commit -> Committing", Atomics.load(control, 0), COMMITTING);
  check("owned commit clears the byte", Atomics.load(interrupt, 0), 0);
  check("late cancel reports Committing", requestCancellation(control, interrupt), ExecutionState.Committing);
}

{
  // Commit must also win from ComputingSuspended: a job whose safe-point
  // resume outruns the view's release legally executes while suspended, and a
  // gate that failed there would misreport a never-cancelled job as cancelled.
  const { control, interrupt } = fresh(COMPUTING_SUSPENDED);
  check("beginCommit wins from suspended", beginCommit(control, interrupt), true);
  check("suspended commit -> Committing", Atomics.load(control, 0), COMMITTING);
}

{
  // A lost commit means the cancel won, and the pending byte must be cleared
  // so it cannot fire inside rollback or reply marshalling.
  const { control, interrupt } = fresh(COMPUTING_OWNED);
  requestCancellation(control, interrupt);
  check("byte armed before the gate", Atomics.load(interrupt, 0), 2);
  check("beginCommit loses to cancel", beginCommit(control, interrupt), false);
  check("lost commit clears the byte", Atomics.load(interrupt, 0), 0);
  check("state stays CancellingOwned", Atomics.load(control, 0), CANCELLING_OWNED);
}

{
  // Suspension hands the byte back clean; resumption of a cancelled job
  // re-arms it.
  const { control, interrupt } = fresh(COMPUTING_OWNED);
  requestCancellation(control, interrupt);
  setInterpreterOwner(control, false, interrupt);
  check("suspend -> CancellingSuspended", Atomics.load(control, 0), CANCELLING_SUSPENDED);
  check("suspend clears the byte", Atomics.load(interrupt, 0), 0);
  setInterpreterOwner(control, true, interrupt);
  check("resume -> CancellingOwned", Atomics.load(control, 0), CANCELLING_OWNED);
  check("resume re-arms the byte", Atomics.load(interrupt, 0), 2);
}

{
  // The damaging negatives: resuming a NON-cancelled job must never arm the
  // byte (every job that yields to a view resumes through this path), and an
  // uncancelled suspension must hand the byte back clean.
  const { control, interrupt } = fresh(COMPUTING_SUSPENDED);
  check("uncancelled job is not cancelling", cancellationRequested(control), false);
  setInterpreterOwner(control, true, interrupt);
  check("uncancelled resume -> ComputingOwned", Atomics.load(control, 0), COMPUTING_OWNED);
  check("uncancelled resume leaves the byte", Atomics.load(interrupt, 0), 0);
  check("owned uncancelled job is not cancelling", cancellationRequested(control), false);
  Atomics.store(interrupt, 0, 2);
  setInterpreterOwner(control, false, interrupt);
  check("uncancelled suspend -> ComputingSuspended", Atomics.load(control, 0), COMPUTING_SUSPENDED);
  check("uncancelled suspend clears the byte", Atomics.load(interrupt, 0), 0);
}

{
  // markExecutionDone reaches Done from every stable state and zeroes the byte.
  for (const start of [COMPUTING_SUSPENDED, COMPUTING_OWNED, CANCELLING_SUSPENDED, CANCELLING_OWNED, COMMITTING, DONE]) {
    const { control, interrupt } = fresh(start);
    Atomics.store(interrupt, 0, 2);
    markExecutionDone(control, interrupt);
    check(`done from state ${start}`, Atomics.load(control, 0), DONE);
    check(`done zeroes the byte from ${start}`, Atomics.load(interrupt, 0), 0);
  }
  const { control, interrupt } = fresh(DONE);
  check("cancel after done reports Done", requestCancellation(control, interrupt), ExecutionState.Done);
}

if (failures > 0) {
  console.error(`${failures} execution-control checks failed`);
  process.exit(1);
}
console.log("all execution-control checks passed");
