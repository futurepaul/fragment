// Reply and attachment actions, invoked from a person's click. Cancelling
// a native share sheet is quiet; unavailable or failed sharing copies.
export async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    return "Copied";
  } catch {
    const field = document.createElement("textarea");
    field.value = text;
    field.style.position = "fixed";
    field.style.opacity = "0";
    document.body.append(field);
    field.select();
    const copied = document.execCommand("copy");
    field.remove();
    return copied ? "Copied" : "Could not copy";
  }
}

export async function share(data) {
  if (navigator.share) {
    try {
      await navigator.share(data);
      return "Shared";
    } catch (err) {
      if (err.name === "AbortError") return null;
    }
  }
  return copy(data.text ?? data.url);
}

export async function feedback(button, action) {
  const result = await action();
  if (!result) return;
  const label = button.getAttribute("aria-label") ?? button.title ?? button.textContent;
  const content = [...button.childNodes].map((n) => n.cloneNode(true));
  button.textContent = result;
  button.setAttribute("aria-label", result);
  setTimeout(() => {
    button.replaceChildren(...content);
    button.setAttribute("aria-label", label);
  }, 1500);
}
