// One request-local atomic state linearizes cancellation, interpreter ownership,
// and commit.
// `PublishingCancellationInterrupt` prevents the worker from handing
// the interpreter to a view until the main thread has published the interrupt byte.

export const enum ExecutionState {
  Computing = 1,
  Cancelling = 2,
  Committing = 3,
  Done = 4,
}

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

const STATE = 0;

export function createExecutionControl(): Int32Array {
  const control = new Int32Array(new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT));
  Atomics.store(control, STATE, ExecutionControlState.ComputingWithoutInterpreter);
  return control;
}

export function executionControl(buffer: SharedArrayBuffer): Int32Array {
  return new Int32Array(buffer);
}

export function requestCancellation(control: Int32Array, interrupt: Uint8Array): ExecutionState {
  for (;;) {
    const state = Atomics.load(control, STATE) as ExecutionControlState;
    if (state === ExecutionControlState.ComputingWithoutInterpreter) {
      if (transition(control, state, ExecutionControlState.CancellingWithoutInterpreter)) {
        return ExecutionState.Computing;
      }
    } else if (state === ExecutionControlState.ComputingWithInterpreter) {
      if (transition(control, state, ExecutionControlState.PublishingCancellationInterrupt)) {
        Atomics.store(interrupt, 0, 2);
        Atomics.store(control, STATE, ExecutionControlState.CancellingWithInterpreter);
        Atomics.notify(control, STATE);
        return ExecutionState.Computing;
      }
    } else if (state === ExecutionControlState.ComputingDuringExternalWait) {
      if (transition(control, state, ExecutionControlState.CancellingDuringExternalWait)) {
        return ExecutionState.Computing;
      }
    } else if (
      state === ExecutionControlState.PublishingCancellationInterrupt ||
      state === ExecutionControlState.CancellingWithoutInterpreter ||
      state === ExecutionControlState.CancellingWithInterpreter ||
      state === ExecutionControlState.CancellingDuringExternalWait
    ) {
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
        Atomics.store(interrupt, 0, 0);
        return true;
      }
    } else if (state === ExecutionControlState.PublishingCancellationInterrupt) {
      Atomics.wait(control, STATE, ExecutionControlState.PublishingCancellationInterrupt);
    } else {
      Atomics.store(interrupt, 0, 0);
      return false;
    }
  }
}

export function cancellationRequested(control: Int32Array): boolean {
  const state = Atomics.load(control, STATE);
  return (
    state === ExecutionControlState.PublishingCancellationInterrupt ||
    state === ExecutionControlState.CancellingWithoutInterpreter ||
    state === ExecutionControlState.CancellingWithInterpreter ||
    state === ExecutionControlState.CancellingDuringExternalWait
  );
}

export function markExecutionDone(control: Int32Array, interrupt: Uint8Array): void {
  for (;;) {
    const state = stableState(control);
    if (transition(control, state, ExecutionControlState.Done)) break;
  }
  Atomics.store(interrupt, 0, 0);
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
        Atomics.store(interrupt, 0, 0);
      } else if (next === ExecutionControlState.CancellingWithInterpreter) {
        Atomics.store(interrupt, 0, 2);
      }
      return;
    }
  }
}

// There is deliberately NO ownership-granting entry point for the dispatch
// path: a request's control word stays ...WithoutInterpreter — where
// requestCancellation cannot arm SIGINT — until Python itself calls
// setInterpreterOwnership through the worker's enterProtectedFrame callback,
// from bridge.call's first protected bytecodes. An interrupt armed any earlier
// would be consumed by Pyodide's coroutine-scheduling plumbing instead of the
// target frame — a transport failure or a stranded Future rather than the
// typed cancelled result.

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
      Atomics.store(interrupt, 0, 0);
    } else if (next === ExecutionControlState.CancellingWithInterpreter) {
      Atomics.store(interrupt, 0, 2);
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

function transition(
  control: Int32Array,
  from: ExecutionControlState,
  to: ExecutionControlState
): boolean {
  return Atomics.compareExchange(control, STATE, from, to) === from;
}

function stableState(control: Int32Array): ExecutionControlState {
  for (;;) {
    const state = Atomics.load(control, STATE) as ExecutionControlState;
    if (state !== ExecutionControlState.PublishingCancellationInterrupt) return state;
    Atomics.wait(control, STATE, ExecutionControlState.PublishingCancellationInterrupt);
  }
}
