// Transition-table checks for the request-local execution control: the atomic
// state machine that linearizes cancellation, interpreter ownership, and
// commit. Pure and synchronous, so it runs under plain bun with no worker —
// each check drives the exported API from a chosen stable state and asserts
// the resulting state and interrupt byte. (`Atomics.wait` paths are exercised
// only from states that never wait, since this runs single-threaded.)
import {
  ExecutionControlState,
  ExecutionState,
  beginCommit,
  cancellationRequested,
  createExecutionControl,
  executionControl,
  markExecutionDone,
  requestCancellation,
  setExternalWait,
  setInterpreterOwnership,
} from "../web/execution_control.js";

let failures = 0;

function check(name: string, actual: unknown, expected: unknown): void {
  if (actual !== expected) {
    failures += 1;
    console.error(`FAIL ${name}: expected ${String(expected)}, got ${String(actual)}`);
  } else {
    console.log(`ok   ${name}`);
  }
}

function fresh(state?: ExecutionControlState): { control: Int32Array; interrupt: Uint8Array } {
  const control = createExecutionControl();
  if (state !== undefined) Atomics.store(control, 0, state);
  const interrupt = new Uint8Array(new SharedArrayBuffer(1));
  return { control, interrupt };
}

{
  // A new job has no interpreter ownership until the worker grants it.
  const { control } = fresh();
  check(
    "initial state is ComputingWithoutInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.ComputingWithoutInterpreter
  );
}

{
  // executionControl re-wraps the same buffer, not a copy.
  const { control } = fresh(ExecutionControlState.ComputingWithInterpreter);
  const rewrapped = executionControl(control.buffer as SharedArrayBuffer);
  check(
    "executionControl shares the buffer",
    Atomics.load(rewrapped, 0),
    ExecutionControlState.ComputingWithInterpreter
  );
}

{
  // Cancelling without interpreter ownership must not write the interrupt byte.
  // The interpreter may be running another lane's Python.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithoutInterpreter);
  const seen = requestCancellation(control, interrupt);
  check("cancel without the interpreter reports Computing", seen, ExecutionState.Computing);
  check(
    "cancel without the interpreter -> CancellingWithoutInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithoutInterpreter
  );
  check("cancel without the interpreter leaves the byte clear", Atomics.load(interrupt, 0), 0);
}

{
  // A cancelling job that owns the interpreter must publish the interrupt byte.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithInterpreter);
  const seen = requestCancellation(control, interrupt);
  check("cancel with the interpreter reports Computing", seen, ExecutionState.Computing);
  check(
    "cancel with the interpreter -> CancellingWithInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithInterpreter
  );
  check("cancel with the interpreter arms the byte", Atomics.load(interrupt, 0), 2);
  check("cancellationRequested sees it", cancellationRequested(control), true);
  const again = requestCancellation(control, interrupt);
  check("second cancel reports Cancelling", again, ExecutionState.Cancelling);
}

{
  // Commit wins while the job owns the interpreter and clears the byte.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithInterpreter);
  Atomics.store(interrupt, 0, 2);
  check("beginCommit wins with the interpreter", beginCommit(control, interrupt), true);
  check(
    "commit with the interpreter -> Committing",
    Atomics.load(control, 0),
    ExecutionControlState.Committing
  );
  check("commit with the interpreter clears the byte", Atomics.load(interrupt, 0), 0);
  check(
    "late cancel reports Committing",
    requestCancellation(control, interrupt),
    ExecutionState.Committing
  );
}

{
  // Commit must also win without interpreter ownership: a job whose safe-point
  // resume outruns the view's release can legally finish in that state.
  // Rejecting the commit there would misreport a never-cancelled job as cancelled.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithoutInterpreter);
  check("beginCommit wins without the interpreter", beginCommit(control, interrupt), true);
  check(
    "commit without the interpreter -> Committing",
    Atomics.load(control, 0),
    ExecutionControlState.Committing
  );
}

{
  // A lost commit means the cancel won, and the pending byte must be cleared
  // so it cannot fire inside rollback or reply marshalling.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithInterpreter);
  requestCancellation(control, interrupt);
  check("byte armed before the gate", Atomics.load(interrupt, 0), 2);
  check("beginCommit loses to cancel", beginCommit(control, interrupt), false);
  check("lost commit clears the byte", Atomics.load(interrupt, 0), 0);
  check(
    "state stays CancellingWithInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithInterpreter
  );
}

{
  // Releasing interpreter ownership clears the byte.
  // Regaining ownership for a cancelled job re-arms it.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithInterpreter);
  requestCancellation(control, interrupt);
  setInterpreterOwnership(control, false, interrupt);
  check(
    "release interpreter -> CancellingWithoutInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithoutInterpreter
  );
  check("releasing the interpreter clears the byte", Atomics.load(interrupt, 0), 0);
  setInterpreterOwnership(control, true, interrupt);
  check(
    "regain interpreter -> CancellingWithInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithInterpreter
  );
  check("regaining the interpreter re-arms the byte", Atomics.load(interrupt, 0), 2);
}

{
  // Granting the interpreter to a non-cancelled job must never arm the byte.
  // Every job that yields to a view later regains ownership through this path.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithoutInterpreter);
  check("uncancelled job is not cancelling", cancellationRequested(control), false);
  setInterpreterOwnership(control, true, interrupt);
  check(
    "uncancelled ownership -> ComputingWithInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.ComputingWithInterpreter
  );
  check("uncancelled ownership leaves the byte clear", Atomics.load(interrupt, 0), 0);
  check(
    "uncancelled job with the interpreter is not cancelling",
    cancellationRequested(control),
    false
  );
  Atomics.store(interrupt, 0, 2);
  setInterpreterOwnership(control, false, interrupt);
  check(
    "uncancelled release -> ComputingWithoutInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.ComputingWithoutInterpreter
  );
  check("uncancelled release clears the byte", Atomics.load(interrupt, 0), 0);
}

{
  // External waits keep cancellation observable without exposing Pyodide's
  // promise resolver to the job's SIGINT.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithInterpreter);
  Atomics.store(interrupt, 0, 2);
  setExternalWait(control, true, false, interrupt);
  check(
    "external wait -> ComputingDuringExternalWait",
    Atomics.load(control, 0),
    ExecutionControlState.ComputingDuringExternalWait
  );
  check("external wait clears the byte", Atomics.load(interrupt, 0), 0);
  check(
    "cancel during external wait reports Computing",
    requestCancellation(control, interrupt),
    ExecutionState.Computing
  );
  check(
    "cancel during external wait -> CancellingDuringExternalWait",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingDuringExternalWait
  );
  check("cancel during external wait leaves the byte clear", Atomics.load(interrupt, 0), 0);
  setInterpreterOwnership(control, true, interrupt);
  check(
    "view release preserves CancellingDuringExternalWait",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingDuringExternalWait
  );
  setExternalWait(control, false, true, interrupt);
  check(
    "external wait ends -> CancellingWithInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithInterpreter
  );
  check("ending the external wait re-arms the byte", Atomics.load(interrupt, 0), 2);
}

{
  // A coroutine can resume before the view's JS finally block releases its
  // ownership bookkeeping, so resumption must preserve suspension in that gap.
  const { control, interrupt } = fresh(ExecutionControlState.ComputingWithInterpreter);
  setExternalWait(control, true, false, interrupt);
  setInterpreterOwnership(control, true, interrupt);
  setExternalWait(control, false, false, interrupt);
  check(
    "ending a wait under a view keeps the interpreter unavailable",
    Atomics.load(control, 0),
    ExecutionControlState.ComputingWithoutInterpreter
  );
  check("ending a wait under a view leaves the byte clear", Atomics.load(interrupt, 0), 0);
}

{
  // markExecutionDone reaches Done from every stable state and zeroes the byte.
  for (const start of [
    ExecutionControlState.ComputingWithoutInterpreter,
    ExecutionControlState.ComputingWithInterpreter,
    ExecutionControlState.CancellingWithoutInterpreter,
    ExecutionControlState.CancellingWithInterpreter,
    ExecutionControlState.Committing,
    ExecutionControlState.Done,
    ExecutionControlState.ComputingDuringExternalWait,
    ExecutionControlState.CancellingDuringExternalWait,
  ]) {
    const { control, interrupt } = fresh(start);
    Atomics.store(interrupt, 0, 2);
    markExecutionDone(control, interrupt);
    check(`done from state ${start}`, Atomics.load(control, 0), ExecutionControlState.Done);
    check(`done zeroes the byte from ${start}`, Atomics.load(interrupt, 0), 0);
  }
  const { control, interrupt } = fresh(ExecutionControlState.Done);
  check(
    "cancel after done reports Done",
    requestCancellation(control, interrupt),
    ExecutionState.Done
  );
}

{
  // Until Python's first protected bytecodes grant ownership through
  // enterProtectedFrame, a request stays ...WithoutInterpreter — where a
  // cancel records the request but can never arm SIGINT. Pyodide's
  // coroutine-scheduling plumbing therefore always runs with the byte clear.
  const { control, interrupt } = fresh();
  const seen = requestCancellation(control, interrupt);
  check("pre-frame cancel reports Computing", seen, ExecutionState.Computing);
  check(
    "pre-frame cancel -> CancellingWithoutInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithoutInterpreter
  );
  check("pre-frame cancel leaves the byte clear", Atomics.load(interrupt, 0), 0);
  // The protected-frame entry then grants ownership; arming a pending
  // cancellation here is correct because the target frame is now executing,
  // and the very next bytecode raises the typed cancellation inside the try.
  setInterpreterOwnership(control, true, interrupt);
  check(
    "protected-frame entry of a cancelled request -> CancellingWithInterpreter",
    Atomics.load(control, 0),
    ExecutionControlState.CancellingWithInterpreter
  );
  check("protected-frame entry of a cancelled request arms the byte", Atomics.load(interrupt, 0), 2);
}

if (failures > 0) {
  console.error(`${failures} execution-control checks failed`);
  process.exit(1);
}
console.log("all execution-control checks passed");
