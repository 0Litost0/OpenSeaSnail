/** Use physical key codes so Option/dead keys and keyboard layouts remain predictable. */
export function shortcutFromKey(event: Pick<KeyboardEvent, "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "isComposing" | "repeat">): string | null {
  if (event.isComposing || event.repeat) return null;
  const modifiers = [event.metaKey && "Command", event.ctrlKey && "Control", event.altKey && "Alt", event.shiftKey && "Shift"].filter(Boolean);
  if (!event.metaKey && !event.ctrlKey && !event.altKey) return null;
  let key: string | undefined;
  if (/^Key[A-Z]$/.test(event.code)) key = event.code.slice(3);
  else if (/^Digit[0-9]$/.test(event.code)) key = event.code.slice(5);
  else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(event.code)) key = event.code;
  else key = ({ Space: "Space", Enter: "Enter", Tab: "Tab", Backspace: "Backspace", Delete: "Delete", ArrowUp: "ArrowUp", ArrowDown: "ArrowDown", ArrowLeft: "ArrowLeft", ArrowRight: "ArrowRight", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown", Minus: "Minus", Equal: "Equal", BracketLeft: "BracketLeft", BracketRight: "BracketRight", Backslash: "Backslash", Semicolon: "Semicolon", Quote: "Quote", Comma: "Comma", Period: "Period", Slash: "Slash", Backquote: "Backquote" } as Record<string, string>)[event.code];
  return key ? [...modifiers, key].join("+") : null;
}

export function displayShortcut(shortcut: string): string {
  return shortcut.split("+").map((part) => ({ Command: "⌘", Control: "⌃", Alt: "⌥", Option: "⌥", Shift: "⇧" } as Record<string, string>)[part] ?? part).join(" ");
}
