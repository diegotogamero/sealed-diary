// All entry text reaches the page through textContent or form values, never
// as HTML: what is written in the diary must not be able to run as code here.

const { invoke, convertFileSrc } = window.__TAURI__.core;

const AUTO_LOCK_MS = 5 * 60 * 1000;

const MESSAGES = {
  "wrong-secret": "Contraseña o clave de recuperación incorrecta.",
  "weak-password": "La contraseña debe tener al menos 10 caracteres.",
  corrupted: "Los datos están dañados o han sido manipulados.",
  "unsupported-version": "Este diario se creó con una versión más reciente de la aplicación.",
  locked: "El diario está bloqueado.",
};

const $ = (id) => document.getElementById(id);
const describe = (error) => MESSAGES[error] ?? `Ha ocurrido un error (${error}).`;

/** The entry open in the editor. `id` is null until first saved. */
let open = { id: null, replaces: [], attachments: [] };
let dirty = false;
let lockTimer;

// ---------------------------------------------------------------- lock screen

const gateForms = ["create-form", "unlock-form", "recover-form", "recovery-shown"];

function showGate(form) {
  $("diary").hidden = true;
  $("gate").hidden = false;
  for (const id of gateForms) $(id).hidden = id !== form;
  $("gate-error").hidden = true;
  $(form).querySelector("input, textarea")?.focus();
}

function gateError(error) {
  $("gate-error").textContent = describe(error);
  $("gate-error").hidden = false;
}

/** Runs a gate form's action with its button disabled; key derivation takes a moment. */
async function submitGate(form, action) {
  const button = form.querySelector("button.primary");
  button.disabled = true;
  $("gate-error").hidden = true;
  try {
    await action();
  } catch (error) {
    gateError(error);
  } finally {
    button.disabled = false;
    for (const field of form.querySelectorAll("input, textarea")) field.value = "";
  }
}

$("create-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const password = $("create-password").value;
  if (password !== $("create-repeat").value) {
    $("gate-error").textContent = "Las contraseñas no coinciden.";
    $("gate-error").hidden = false;
    return;
  }
  submitGate(event.target, async () => {
    $("recovery-key").textContent = await invoke("create_vault", { password });
    $("recovery-saved").checked = false;
    $("recovery-done").disabled = true;
    showGate("recovery-shown");
  });
});

$("recovery-saved").addEventListener("change", (event) => {
  $("recovery-done").disabled = !event.target.checked;
});

$("recovery-done").addEventListener("click", () => {
  $("recovery-key").textContent = "";
  enterDiary();
});

$("unlock-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const password = $("unlock-password").value;
  submitGate(event.target, async () => {
    await invoke("unlock", { password });
    await enterDiary();
  });
});

$("recover-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const recoveryKey = $("recover-key").value;
  const newPassword = $("recover-password").value;
  submitGate(event.target, async () => {
    await invoke("recover", { recoveryKey, newPassword });
    await enterDiary();
  });
});

$("show-recover").addEventListener("click", () => showGate("recover-form"));
$("show-unlock").addEventListener("click", () => showGate("unlock-form"));

// ---------------------------------------------------------------------- diary

async function enterDiary() {
  $("gate").hidden = true;
  $("diary").hidden = false;
  newEntry();
  await refresh();
  armAutoLock();
}

async function lockDiary() {
  clearTimeout(lockTimer);
  await invoke("lock");
  // Leave nothing readable in the page once the key is gone.
  $("entries").replaceChildren();
  newEntry();
  showGate("unlock-form");
}

function armAutoLock() {
  clearTimeout(lockTimer);
  if ($("diary").hidden) return;
  lockTimer = setTimeout(async () => {
    // Keep what was being written rather than lose it to the timer.
    if (dirty) await save().catch(() => {});
    await lockDiary();
  }, AUTO_LOCK_MS);
}

for (const activity of ["pointerdown", "keydown"]) {
  document.addEventListener(activity, armAutoLock, { passive: true });
}

function today() {
  const now = new Date();
  const pad = (n) => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

function setStatus(text) {
  $("editor-status").textContent = text;
}

function editorError(error) {
  $("editor-error").textContent = error ? describe(error) : "";
  $("editor-error").hidden = !error;
}

function fill(content) {
  $("entry-date").value = content.date;
  $("entry-title").value = content.title;
  $("entry-body").value = content.body;
  $("entry-tags").value = content.tags.join(", ");
  open.attachments = [...content.attachments];
  renderAttachments();
}

function newEntry() {
  open = { id: null, replaces: [], attachments: [] };
  fill({ date: today(), title: "", body: "", tags: [], attachments: [] });
  $("conflicts").hidden = true;
  $("delete-entry").hidden = true;
  dirty = false;
  setStatus("");
  editorError(null);
  markCurrent();
}

function openEntry(state) {
  open = { id: state.current.id, replaces: [state.current, ...state.conflicts].map((e) => e.revision) };
  fill(state.current.content);
  renderConflicts(state);
  $("delete-entry").hidden = false;
  dirty = false;
  setStatus("");
  editorError(null);
  markCurrent();
}

function renderConflicts(state) {
  $("conflicts").hidden = state.conflicts.length === 0;
  $("conflict-list").replaceChildren(
    ...[state.current, ...state.conflicts].map((version, index) => {
      const button = document.createElement("button");
      button.type = "button";
      const saved = new Date(version.saved_at_ms).toLocaleString();
      button.textContent = `Ver la versión guardada el ${saved}${index === 0 ? " (la más reciente)" : ""}`;
      button.addEventListener("click", () => {
        fill(version.content);
        dirty = true;
      });
      const item = document.createElement("li");
      item.append(button);
      return item;
    }),
  );
}

function mediaUrl(attachment) {
  // The id goes through convertFileSrc so the URL takes the right form on
  // every platform; the type is appended as plain path segments.
  return `${convertFileSrc(attachment.object, "media")}/${attachment.mime}`;
}

function renderAttachments() {
  $("attachments").replaceChildren(
    ...open.attachments.map((attachment) => {
      const item = document.createElement("li");
      const kind = attachment.mime.split("/")[0];
      if (kind === "image") {
        const image = document.createElement("img");
        image.src = mediaUrl(attachment);
        image.alt = attachment.name;
        image.loading = "lazy";
        item.append(image);
      } else if (kind === "video" || kind === "audio") {
        const player = document.createElement(kind);
        player.src = mediaUrl(attachment);
        player.controls = true;
        player.preload = "metadata";
        item.append(player);
      }
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = attachment.name;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.textContent = "Quitar";
      remove.setAttribute("aria-label", `Quitar ${attachment.name}`);
      remove.addEventListener("click", () => {
        open.attachments = open.attachments.filter((a) => a.object !== attachment.object);
        dirty = true;
        renderAttachments();
      });
      item.append(name, remove);
      return item;
    }),
  );
}

let listing = { entries: [], unreadable: 0 };

function markCurrent() {
  for (const button of $("entries").querySelectorAll("button")) {
    button.setAttribute("aria-current", String(button.dataset.id === open.id));
  }
}

async function refresh() {
  listing = await invoke("list_entries");
  $("empty").hidden = listing.entries.length > 0;
  $("unreadable").hidden = listing.unreadable === 0;
  $("unreadable").textContent =
    `${listing.unreadable} entrada(s) no se pueden leer: están dañadas o vienen de una versión más reciente.`;
  $("entries").replaceChildren(
    ...listing.entries.map((state) => {
      const { id, content } = state.current;
      const date = document.createElement("time");
      date.dateTime = content.date;
      date.textContent = content.date + (state.conflicts.length ? " · conflicto" : "");
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = content.title || content.body.slice(0, 60) || "Sin título";
      const button = document.createElement("button");
      button.type = "button";
      button.dataset.id = id;
      button.append(date, name);
      button.addEventListener("click", async () => {
        if (!(await canLeave())) return;
        openEntry(state);
        $("diary").dataset.pane = "editor";
      });
      const item = document.createElement("li");
      item.append(button);
      return item;
    }),
  );
  markCurrent();
}

function ask(text, action) {
  $("confirm-text").textContent = text;
  $("confirm-ok").textContent = action;
  const dialog = $("confirm");
  dialog.returnValue = "cancel";
  dialog.showModal();
  return new Promise((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "ok"), { once: true });
  });
}

async function canLeave() {
  return !dirty || ask("Hay cambios sin guardar en esta entrada.", "Descartar cambios");
}

async function save() {
  const content = {
    date: $("entry-date").value,
    title: $("entry-title").value.trim(),
    body: $("entry-body").value,
    tags: $("entry-tags").value.split(",").map((tag) => tag.trim()).filter(Boolean),
    attachments: open.attachments,
  };
  const saved = await invoke("save_entry", { request: { id: open.id, replaces: open.replaces, content } });
  await refresh();
  const state = listing.entries.find((entry) => entry.current.id === saved.id);
  if (state) openEntry(state);
  setStatus("Guardado.");
}

$("editor").addEventListener("input", () => {
  dirty = true;
  setStatus("");
});

$("editor").addEventListener("submit", async (event) => {
  event.preventDefault();
  $("save-entry").disabled = true;
  editorError(null);
  try {
    await save();
  } catch (error) {
    editorError(error);
  } finally {
    $("save-entry").disabled = false;
  }
});

$("attach").addEventListener("click", async () => {
  $("attach").disabled = true;
  setStatus("Cifrando…");
  editorError(null);
  try {
    const added = await invoke("attach_files");
    if (added.length) {
      open.attachments.push(...added);
      dirty = true;
      renderAttachments();
    }
    setStatus(added.length ? "Adjunto añadido. Guarda la entrada para conservarlo." : "");
  } catch (error) {
    setStatus("");
    editorError(error);
  } finally {
    $("attach").disabled = false;
  }
});

$("delete-entry").addEventListener("click", async () => {
  if (!(await ask("Se eliminará esta entrada y sus adjuntos. No se puede deshacer.", "Eliminar"))) return;
  try {
    await invoke("delete_entry", { id: open.id, replaces: open.replaces });
    newEntry();
    await refresh();
    $("diary").dataset.pane = "list";
  } catch (error) {
    editorError(error);
  }
});

$("new-entry").addEventListener("click", async () => {
  if (!(await canLeave())) return;
  newEntry();
  $("diary").dataset.pane = "editor";
  $("entry-title").focus();
});

$("back").addEventListener("click", async () => {
  if (await canLeave()) {
    dirty = false;
    $("diary").dataset.pane = "list";
  }
});

$("lock").addEventListener("click", async () => {
  if (await canLeave()) await lockDiary();
});

// ---------------------------------------------------------------------- start

const { exists, unlocked } = await invoke("status");
if (unlocked) await enterDiary();
else showGate(exists ? "unlock-form" : "create-form");
