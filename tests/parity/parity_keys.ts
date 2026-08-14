// One translation from the harness's key notation to Playwright's, shared by the
// tier-2 key probe and the tier-3 scenario driver.
//
// The notation is the one both help pages already use — `q`, `A`, `<space>`, `<f1>`,
// `<shift> + <tab>`, `<ctrl> + o`, and Qt's two-stroke chords like `s, c` — so the
// scenario files, the Qt inventory and the web driver all say the same thing.

/// What to do to the keyboard for one token: `press` for a named key or a combination,
/// `type` for a character whose key name depends on the layout.
export interface KeyAction {
  kind: "press" | "type";
  value: string;
}

const NAMED: Record<string, string> = {
  space: "Space",
  backspace: "Backspace",
  delete: "Delete",
  escape: "Escape",
  tab: "Tab",
  enter: "Enter",
  pageup: "PageUp",
  pagedown: "PageDown",
  up: "ArrowUp",
  down: "ArrowDown",
  left: "ArrowLeft",
  right: "ArrowRight",
};

const MODIFIERS: Record<string, string> = {
  shift: "Shift",
  ctrl: "Control",
  alt: "Alt",
  meta: "Meta",
};

function namedKey(word: string): string {
  if (word in NAMED) return NAMED[word]!;
  if (/^f\d+$/.test(word)) return word.toUpperCase();
  return word;
}

/// Translate one stroke — everything between the commas of a chord.
function stroke(part: string): KeyAction {
  // `+` is a key snuffler binds (show one more track), so it cannot be split on as a
  // modifier separator first.
  if (part.length === 1) return character(part);
  const pieces = part.split("+").map((piece) => piece.trim());
  const modifiers: string[] = [];
  let base = "";
  for (const piece of pieces) {
    const bare = piece.replace(/^<|>$/g, "");
    if (piece.startsWith("<") && bare in MODIFIERS) {
      modifiers.push(MODIFIERS[bare]!);
    } else {
      base = piece.startsWith("<") ? namedKey(bare) : piece;
    }
  }
  if (modifiers.length) return { kind: "press", value: [...modifiers, base].join("+") };
  return base.length === 1 ? character(base) : { kind: "press", value: base };
}

/// One unmodified character.
///
/// `press("A")` is the A *key*, which without a modifier delivers "a" — and snuffler's
/// `A` and `a` are different commands. Punctuation is worse: which key produces ":" or
/// "?" is a layout question, so let the browser work it out from the character.
function character(base: string): KeyAction {
  if (base >= "A" && base <= "Z") return { kind: "press", value: `Shift+${base}` };
  if (!/[a-z0-9]/.test(base)) return { kind: "type", value: base };
  return { kind: "press", value: base };
}

export function keyActions(token: string): KeyAction[] {
  return token
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean)
    .map(stroke);
}

/// Send one token to the page.
export async function pressKey(
  keyboard: { press(key: string): Promise<void>; type(text: string): Promise<void> },
  token: string
): Promise<void> {
  for (const action of keyActions(token)) {
    if (action.kind === "press") await keyboard.press(action.value);
    else await keyboard.type(action.value);
  }
}
