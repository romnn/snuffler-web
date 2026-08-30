// One request-local atomic state linearizes cancellation, interpreter ownership,
// and commit. `InterruptingOwned` prevents the worker from handing ownership to a
// view until the main thread has published the interrupt byte.

export const enum ExecutionState {
  Computing = 1,
  Cancelling = 2,
  Committing = 3,
  Done = 4,
}

const enum ControlState {
  ComputingSuspended = 1,
  ComputingOwned = 2,
  InterruptingOwned = 3,
  CancellingSuspended = 4,
  CancellingOwned = 5,
  Committing = 6,
  Done = 7,
}

const STATE = 0;

export function createExecutionControl(): Int32Array {
  const control = new Int32Array(new SharedArrayBuffer(Int32Array.BYTES_PER_ELEMENT));
  Atomics.store(control, STATE, ControlState.ComputingSuspended);
  return control;
}

export function executionControl(buffer: SharedArrayBuffer): Int32Array {
  return new Int32Array(buffer);
}

export function requestCancellation(control: Int32Array, interrupt: Uint8Array): ExecutionState {
  for (;;) {
    const state = Atomics.load(control, STATE) as ControlState;
    if (state === ControlState.ComputingSuspended) {
      if (transition(control, state, ControlState.CancellingSuspended)) {
        return ExecutionState.Computing;
      }
    } else if (state === ControlState.ComputingOwned) {
      if (transition(control, state, ControlState.InterruptingOwned)) {
        Atomics.store(interrupt, 0, 2);
        Atomics.store(control, STATE, ControlState.CancellingOwned);
        Atomics.notify(control, STATE);
        return ExecutionState.Computing;
      }
    } else if (
      state === ControlState.InterruptingOwned ||
      state === ControlState.CancellingSuspended ||
      state === ControlState.CancellingOwned
    ) {
      return ExecutionState.Cancelling;
    } else if (state === ControlState.Committing) {
      return ExecutionState.Committing;
    } else {
      return ExecutionState.Done;
    }
  }
}

// Commit races only *cancellation*, never ownership: a job whose safe-point
// resume outruns the view's release legally executes while ComputingSuspended,
// so both computing states are valid commit sources. On loss the cancel has
// already won; clearing the interrupt byte here keeps a not-yet-consumed
// interrupt from firing later inside rollback or reply marshalling.
export function beginCommit(control: Int32Array, interrupt: Uint8Array): boolean {
  for (;;) {
    const state = Atomics.load(control, STATE) as ControlState;
    if (state === ControlState.ComputingSuspended || state === ControlState.ComputingOwned) {
      if (transition(control, state, ControlState.Committing)) {
        Atomics.store(interrupt, 0, 0);
        return true;
      }
    } else if (state === ControlState.InterruptingOwned) {
      Atomics.wait(control, STATE, ControlState.InterruptingOwned);
    } else {
      Atomics.store(interrupt, 0, 0);
      return false;
    }
  }
}

export function cancellationRequested(control: Int32Array): boolean {
  const state = Atomics.load(control, STATE);
  return (
    state === ControlState.InterruptingOwned ||
    state === ControlState.CancellingSuspended ||
    state === ControlState.CancellingOwned
  );
}

export function markExecutionDone(control: Int32Array, interrupt: Uint8Array): void {
  for (;;) {
    const state = stableState(control);
    if (transition(control, state, ControlState.Done)) break;
  }
  Atomics.store(interrupt, 0, 0);
}

export function setInterpreterOwner(
  control: Int32Array,
  owns: boolean,
  interrupt: Uint8Array
): void {
  for (;;) {
    const state = stableState(control);
    const next = ownershipState(state, owns);
    if (next === state || transition(control, state, next)) {
      if (!owns) {
        Atomics.store(interrupt, 0, 0);
      } else if (next === ControlState.CancellingOwned) {
        Atomics.store(interrupt, 0, 2);
      }
      return;
    }
  }
}

function ownershipState(state: ControlState, owns: boolean): ControlState {
  if (owns && state === ControlState.ComputingSuspended) return ControlState.ComputingOwned;
  if (owns && state === ControlState.CancellingSuspended) return ControlState.CancellingOwned;
  if (!owns && state === ControlState.ComputingOwned) return ControlState.ComputingSuspended;
  if (!owns && state === ControlState.CancellingOwned) return ControlState.CancellingSuspended;
  return state;
}

function transition(control: Int32Array, from: ControlState, to: ControlState): boolean {
  return Atomics.compareExchange(control, STATE, from, to) === from;
}

function stableState(control: Int32Array): ControlState {
  for (;;) {
    const state = Atomics.load(control, STATE) as ControlState;
    if (state !== ControlState.InterruptingOwned) return state;
    Atomics.wait(control, STATE, ControlState.InterruptingOwned);
  }
}
