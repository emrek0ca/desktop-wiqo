// The window's own question dialog. The webview's confirm()/alert() never show in the desktop app, so a question asked that way silently
// answered "no" (signing out did nothing). `await WiqoDialog.confirm({ title, message, confirm, danger })` → true / false.
(() => {
  let open = null;

  function close(answer) {
    if (!open) return;
    const { root, resolve } = open;
    open = null;
    root.remove();
    resolve(answer);
  }

  function confirm({ title, message = "", confirm: confirmLabel = "Tamam", cancel = "Vazgeç", danger = false }) {
    close(false);
    return new Promise((resolve) => {
      const root = document.createElement("div");
      root.className = "modal dialog";
      const card = document.createElement("div");
      card.className = "modal-card dialog-card";
      card.setAttribute("role", "alertdialog");
      const heading = document.createElement("h2");
      heading.textContent = title;
      card.append(heading);
      if (message) {
        const text = document.createElement("p");
        text.className = "muted";
        text.textContent = message;
        card.append(text);
      }
      const actions = document.createElement("div");
      actions.className = "modal-actions";
      const spacer = document.createElement("span");
      spacer.className = "spacer";
      const no = document.createElement("button");
      no.className = "secondary";
      no.textContent = cancel;
      no.onclick = () => close(false);
      const yes = document.createElement("button");
      yes.className = danger ? "primary danger-fill" : "primary";
      yes.textContent = confirmLabel;
      yes.onclick = () => close(true);
      actions.append(spacer, no, yes);
      card.append(actions);
      root.append(card);
      root.onclick = (event) => {
        if (event.target === root) close(false);
      };
      document.body.append(root);
      open = { root, resolve };
      yes.focus();
    });
  }

  document.addEventListener("keydown", (event) => {
    if (!open) return;
    if (event.key === "Escape") {
      event.stopImmediatePropagation();
      close(false);
    }
    if (event.key === "Enter") {
      event.preventDefault();
      close(true);
    }
  }, true);

  window.WiqoDialog = { confirm };
})();
