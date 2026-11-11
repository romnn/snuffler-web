// The cancellation state machine shared by the main thread and the Pyodide worker.
//
// Each request carries one Int32 in a SharedArrayBuffer (its control word).
// Every change to it is a compare-and-swap, so cancellation, interpreter ownership
// and commit are linearized: exactly one of "cancelled" and "committed" wins.
//
// A request "owns the interpreter" while its Python frame is the one executing.
// Only then may cancellation arm Pyodide's interrupt byte (SIGINT); armed at any
// other time, the KeyboardInterrupt would land in someone else's frame.
//
// `PublishingCancellationInterrupt` is the short window in which the main thread
// is writing the interrupt byte.
// The worker waits it out before handing the interpreter to a view.

// What a cancellation found the request doing, as requestCancellation reports it.
export const enum ExecutionState {
  Computing = 1,
  Cancelling = 2,
  Committing = 3,
  Done = 4,
}

// The values of the control word.
export const enum ExecutionControlState {
  ComputingWithoutInterpreter = 1,
  ComputingWithInterpreter = 2,
  PublishingCancellationInterrupt = 3,
  CancellingWithoutInterpreter = 4,
  CancellingWithInterpreter = 5,
  Committing = 6,
  Done = 7,
  ComputingDuringExternalWait = 8,
  CancellingDuringExternalWait = 9,
}

// The control word's only slot.
const STATE = 0;

// Pyodide's interrupt buffer holds the pending signal number; 0 means none.
const SIGINT = 2;
const NO_SIGNAL = 0;

function armInterrupt(interrupt: Uint8Array) {
  Atomics.store(interrupt, 0, SIGINT);
}

function clearInterrupt(interrupt: Uint8Array) {
  Atomics.store(interrupt, 0, NO_SIGNAL);
}

export function createExecutionControl(): Int32Array {
  const control = new Int32Array(new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT));
  Atomics.store(control, STATE, ExecutionControlState.ComputingWithoutInterpreter);
  return control;
}

export function executionControl(buffer: SharedArrayBuffer): Int32Array {
  return new Int32Array(buffer);
}

// Called on the main thread.
// Returns the state the request was in when cancellation reached it.
export function requestCancellation(control: Int32Array, interrupt: Uint8Array): ExecutionState {
  for (;;) {
    const state = Atomics.load(control, STATE) as ExecutionControlState;
    if (state === ExecutionControlState.ComputingWithoutInterpreter) {
      if (transition(control, state, ExecutionControlState.CancellingWithoutInterpreter)) {
        return ExecutionState.Computing;
      }
    } else if (state === ExecutionControlState.ComputingWithInterpreter) {
      if (transition(control, state, ExecutionControlState.PublishingCancellationInterrupt)) {
        armInterrupt(interrupt);
        Atomics.store(control, STATE, ExecutionControlState.CancellingWithInterpreter);
        Atomics.notify(control, STATE);
        return ExecutionState.Computing;
      }
    } else if (state === ExecutionControlState.ComputingDuringExternalWait) {
      if (transition(control, state, ExecutionControlState.CancellingDuringExternalWait)) {
        return ExecutionState.Computing;
      }
    } else if (isCancellingState(state)) {
      return ExecutionState.Cancelling;
    } else if (state === ExecutionControlState.Committing) {
      return ExecutionState.Committing;
    } else {
      return ExecutionState.Done;
    }
  }
}

// Commit races only *cancellation*, never ownership.
// A job whose safe-point resume outruns the view's release legally executes
// while ComputingWithoutInterpreter, so both computing states are valid commit
// sources.
// On loss the cancellation request has already won; clearing the interrupt
// byte here keeps a not-yet-consumed interrupt from firing later inside rollback
// or reply marshalling.
export function beginCommit(control: Int32Array, interrupt: Uint8Array): boolean {
  for (;;) {
    const state = Atomics.load(control, STATE) as ExecutionControlState;
    if (
      state === ExecutionControlState.ComputingWithoutInterpreter ||
      state === ExecutionControlState.ComputingWithInterpreter
    ) {
      if (transition(control, state, ExecutionControlState.Committing)) {
        clearInterrupt(interrupt);
        return true;
      }
    } else if (state === ExecutionControlState.PublishingCancellationInterrupt) {
      Atomics.wait(control, STATE, ExecutionControlState.PublishingCancellationInterrupt);
    } else {
      clearInterrupt(interrupt);
      return false;
    }
  }
}

// The one definition of "cancellation has been requested and not resolved":
// requestCancellation's already-cancelling arm and cancellationRequested must
// classify identically, or a future state could join one list and not the other.
function isCancellingState(state: ExecutionControlState): boolean {
  return (
    state === ExecutionControlState.PublishingCancellationInterrupt ||
    state === ExecutionControlState.CancellingWithoutInterpreter ||
    state === ExecutionControlState.CancellingWithInterpreter ||
    state === ExecutionControlState.CancellingDuringExternalWait
  );
}

export function cancellationRequested(control: Int32Array): boolean {
  return isCancellingState(Atomics.load(control, STATE) as ExecutionControlState);
}

// The request has replied; any interrupt still pending is now stale.
export function markExecutionDone(control: Int32Array, interrupt: Uint8Array): void {
  for (;;) {
    const state = stableState(control);
    if (transition(control, state, ExecutionControlState.Done)) break;
  }
  clearInterrupt(interrupt);
}

export function setInterpreterOwnership(
  control: Int32Array,
  jobOwnsInterpreter: boolean,
  interrupt: Uint8Array
): void {
  for (;;) {
    const state = stableState(control);
    const next = interpreterOwnershipState(state, jobOwnsInterpreter);
    if (next === state || transition(control, state, next)) {
      if (!jobOwnsInterpreter) {
        clearInterrupt(interrupt);
      } else if (next === ExecutionControlState.CancellingWithInterpreter) {
        armInterrupt(interrupt);
      }
      return;
    }
  }
}

// There is deliberately no ownership-granting entry point for the dispatch path.
// A request's control word stays ...WithoutInterpreter, where requestCancellation
// cannot arm SIGINT, until Python itself calls setInterpreterOwnership through the
// worker's enterProtectedFrame callback, from service.call's first protected
// bytecodes.
// An interrupt armed any earlier would be consumed by Pyodide's
// coroutine-scheduling plumbing instead of the target frame: a transport failure or
// a stranded Future rather than the typed cancelled result.

// External promises re-enter Python through Pyodide's Future resolver.
// Keep SIGINT disarmed until the protected job coroutine is executing again,
// or KeyboardInterrupt can land in the resolver and strand its Future.
export function setExternalWait(
  control: Int32Array,
  waiting: boolean,
  jobOwnsInterpreter: boolean,
  interrupt: Uint8Array
): void {
  for (;;) {
    const state = stableState(control);
    const next = externalWaitState(state, waiting, jobOwnsInterpreter);
    if (next !== state && !transition(control, state, next)) continue;
    if (waiting || !jobOwnsInterpreter) {
      clearInterrupt(interrupt);
    } else if (next === ExecutionControlState.CancellingWithInterpreter) {
      armInterrupt(interrupt);
    }
    return;
  }
}

function interpreterOwnershipState(
  state: ExecutionControlState,
  jobOwnsInterpreter: boolean
): ExecutionControlState {
  if (jobOwnsInterpreter && state === ExecutionControlState.ComputingWithoutInterpreter) {
    return ExecutionControlState.ComputingWithInterpreter;
  }
  if (jobOwnsInterpreter && state === ExecutionControlState.CancellingWithoutInterpreter) {
    return ExecutionControlState.CancellingWithInterpreter;
  }
  if (!jobOwnsInterpreter && state === ExecutionControlState.ComputingWithInterpreter) {
    return ExecutionControlState.ComputingWithoutInterpreter;
  }
  if (!jobOwnsInterpreter && state === ExecutionControlState.CancellingWithInterpreter) {
    return ExecutionControlState.CancellingWithoutInterpreter;
  }
  return state;
}

function externalWaitState(
  state: ExecutionControlState,
  waiting: boolean,
  jobOwnsInterpreter: boolean
): ExecutionControlState {
  if (waiting) {
    if (
      state === ExecutionControlState.ComputingWithoutInterpreter ||
      state === ExecutionControlState.ComputingWithInterpreter
    ) {
      return ExecutionControlState.ComputingDuringExternalWait;
    }
    if (
      state === ExecutionControlState.CancellingWithoutInterpreter ||
      state === ExecutionControlState.CancellingWithInterpreter
    ) {
      return ExecutionControlState.CancellingDuringExternalWait;
    }
    return state;
  }
  if (state === ExecutionControlState.ComputingDuringExternalWait) {
    return jobOwnsInterpreter
      ? ExecutionControlState.ComputingWithInterpreter
      : ExecutionControlState.ComputingWithoutInterpreter;
  }
  if (state === ExecutionControlState.CancellingDuringExternalWait) {
    return jobOwnsInterpreter
      ? ExecutionControlState.CancellingWithInterpreter
      : ExecutionControlState.CancellingWithoutInterpreter;
  }
  return state;
}

// One compare-and-swap step; false when another thread moved the state first.
function transition(
  control: Int32Array,
  from: ExecutionControlState,
  to: ExecutionControlState
): boolean {
  return Atomics.compareExchange(control, STATE, from, to) === from;
}

// The current state, after waiting out a cancellation that is mid-publish.
function stableState(control: Int32Array): ExecutionControlState {
  for (;;) {
    const state = Atomics.load(control, STATE) as ExecutionControlState;
    if (state !== ExecutionControlState.PublishingCancellationInterrupt) return state;
    Atomics.wait(control, STATE, ExecutionControlState.PublishingCancellationInterrupt);
  }
}
