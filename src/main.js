const { invoke } = window.__TAURI__.core;
const $ = (sel) => document.querySelector(sel);
const isWindows = navigator.userAgent.includes("Windows");

let state = { mounts: [], statuses: [], remotes: [] };
let providers = null;

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.add("show");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => t.classList.remove("show"), 2500);
}

async function attempt(fn, errorEl) {
  if (errorEl) errorEl.textContent = "";
  try {
    return await fn();
  } catch (e) {
    if (errorEl) errorEl.textContent = String(e);
    else toast(String(e));
    throw e;
  }
}

// ---------- tabs ----------

document.querySelectorAll(".tab").forEach((btn) =>
  btn.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((b) => b.classList.toggle("active", b === btn));
    document.querySelectorAll(".panel").forEach((p) => p.classList.toggle("active", p.id === btn.dataset.tab));
    if (btn.dataset.tab === "remotes") loadRemotesTab();
  }),
);

// ---------- overview ----------

async function refresh() {
  const o = await invoke("overview");
  state.mounts = o.mounts;
  state.statuses = o.statuses;
  renderBanners(o);
  renderDrives();
  renderSettings(o);
  state.remotes = await invoke("list_remotes").catch(() => []);
  renderRemotes();
}

function renderBanners(o) {
  const parts = [];
  for (const m of o.missing) {
    parts.push(`<div class="banner"><span><strong>${esc(m.name)} is not installed.</strong> ${esc(m.why)}</span>
      <button data-url="${esc(m.url)}">Download</button></div>`);
  }
  if (!o.rclone_found) {
    parts.push(`<div class="banner"><span><strong>rclone not found.</strong> Set its location in Settings.</span></div>`);
  }
  if (o.system_conf) {
    parts.push(`<div class="banner"><span>Found an existing rclone config at <code>${esc(o.system_conf)}</code>. Copy its remotes into clonemount?</span>
      <button class="primary" id="import-system">Import remotes</button></div>`);
  }
  $("#banners").innerHTML = parts.join("");
  $("#banners").querySelectorAll("[data-url]").forEach((b) =>
    b.addEventListener("click", () => invoke("open_url", { url: b.dataset.url })),
  );
  $("#import-system")?.addEventListener("click", async () => {
    await attempt(() => invoke("import_system_conf"));
    toast("Remotes imported");
    refresh();
  });
}

// ---------- drives ----------

const STATE_LABEL = {
  mounted: "Mounted",
  mounting: "Mounting…",
  stopping: "Unmounting…",
  error: "Failed, retrying",
  unmounted: "Not mounted",
};

function renderDrives() {
  const list = $("#drive-list");
  if (!state.mounts.length) {
    list.innerHTML = `<div class="empty">No drives yet. Click <strong>Add drive</strong> to mount one of your rclone remotes.</div>`;
    return;
  }
  list.innerHTML = state.mounts
    .map((m) => {
      const s = state.statuses.find((x) => x.id === m.id) || { state: "unmounted" };
      const active = ["mounted", "mounting", "error"].includes(s.state);
      const where = s.target || m.target || "automatic location";
      return `<div class="item" data-id="${esc(m.id)}">
        <div class="dot ${s.state}" title="${STATE_LABEL[s.state]}"></div>
        <div class="info">
          <div class="title">${esc(m.name || m.remote)}</div>
          <div class="sub">${esc(m.remote)} → ${esc(where)} · ${STATE_LABEL[s.state]}${m.auto_mount ? "" : " · manual"}</div>
          ${s.state === "error" && s.message ? `<div class="msg">${esc(s.message)}</div>` : ""}
        </div>
        <div class="actions">
          ${s.state === "mounted" ? `<button data-act="open">Open</button>` : ""}
          ${s.state === "error" ? `<button data-act="log">Log</button>` : ""}
          <button data-act="${active ? "unmount" : "mount"}" ${s.state === "stopping" ? "disabled" : ""}>${active ? "Unmount" : "Mount"}</button>
          <button data-act="edit">Edit</button>
        </div>
      </div>`;
    })
    .join("");
}

$("#drive-list").addEventListener("click", async (e) => {
  const btn = e.target.closest("button[data-act]");
  if (!btn) return;
  const id = btn.closest(".item").dataset.id;
  const mount = state.mounts.find((m) => m.id === id);
  const status = state.statuses.find((s) => s.id === id);
  switch (btn.dataset.act) {
    case "mount":
    case "unmount":
      await attempt(() => invoke("set_mounted", { id, mounted: btn.dataset.act === "mount" }));
      setTimeout(pollStatuses, 300);
      break;
    case "open":
      await attempt(() => invoke("open_target", { path: status.target }));
      break;
    case "log":
      await attempt(() => invoke("open_log", { id }));
      break;
    case "edit":
      openDriveDialog(mount);
      break;
  }
});

async function pollStatuses() {
  state.statuses = await invoke("statuses");
  // Don't redraw under an open dialog's feet; it doesn't show statuses anyway.
  renderDrives();
}
setInterval(() => {
  if (!document.hidden) pollStatuses();
}, 2000);

// ---------- drive dialog ----------

let editing = null;

$("#target-hint").textContent = isWindows
  ? "A drive letter such as X:. Leave empty to use a free one."
  : "A folder. Leave empty to use ~/clonemount/<name>.";

function openDriveDialog(mount) {
  if (!state.remotes.length) {
    toast("Add or import a remote first");
    $('.tab[data-tab="remotes"]').click();
    return;
  }
  editing = mount || null;
  const f = $("#drive-form");
  const m = mount || { name: "", remote: "", target: "", auto_mount: true, read_only: false, cache_mode: "full", extra_args: "" };
  const [remoteName, ...rest] = m.remote.split(":");
  f.remote_name.innerHTML = state.remotes
    .map((r) => `<option value="${esc(r.name)}">${esc(r.name)} (${esc(r.kind)})</option>`)
    .join("");
  if (remoteName && !state.remotes.some((r) => r.name === remoteName)) {
    f.remote_name.insertAdjacentHTML("beforeend", `<option value="${esc(remoteName)}">${esc(remoteName)} (missing)</option>`);
  }
  f.remote_name.value = remoteName || state.remotes[0].name;
  f.remote_path.value = rest.join(":");
  f.name.value = m.name;
  f.target.value = m.target;
  f.cache_mode.value = m.cache_mode || "full";
  f.auto_mount.checked = m.auto_mount;
  f.read_only.checked = m.read_only;
  f.extra_args.value = m.extra_args;
  $("#drive-title").textContent = mount ? "Edit drive" : "Add drive";
  $("#delete-drive").hidden = !mount;
  $("#drive-error").textContent = "";
  $("#drive-dialog").showModal();
}

$("#add-drive").addEventListener("click", () => openDriveDialog(null));

$("#drive-form").addEventListener("submit", async (e) => {
  if (e.submitter?.value !== "save") return;
  e.preventDefault();
  const f = e.target;
  const mount = {
    id: editing?.id || "",
    name: f.name.value.trim() || f.remote_name.value,
    remote: `${f.remote_name.value}:${f.remote_path.value.trim().replace(/^\/+/, "")}`,
    target: f.target.value.trim(),
    auto_mount: f.auto_mount.checked,
    read_only: f.read_only.checked,
    cache_mode: f.cache_mode.value,
    extra_args: f.extra_args.value.trim(),
  };
  const saved = await attempt(() => invoke("save_mount", { mount }), $("#drive-error"));
  const wasMounted = ["mounted", "mounting", "error"].includes(state.statuses.find((s) => s.id === saved.id)?.state);
  if (!editing && saved.auto_mount) {
    await invoke("set_mounted", { id: saved.id, mounted: true });
  } else if (wasMounted) {
    // Remount so changed options take effect.
    await invoke("set_mounted", { id: saved.id, mounted: false });
    setTimeout(() => invoke("set_mounted", { id: saved.id, mounted: true }), 3000);
    toast("Remounting with the new settings");
  }
  $("#drive-dialog").close();
  refresh();
});

$("#delete-drive").addEventListener("click", async () => {
  if (!editing || !(await invoke("ask", { message: `Delete "${editing.name}"? It will be unmounted. Files in the remote are not touched.` }))) return;
  await attempt(() => invoke("delete_mount", { id: editing.id }), $("#drive-error"));
  $("#drive-dialog").close();
  refresh();
});

// ---------- remotes ----------

function renderRemotes() {
  const list = $("#remote-list");
  if (!state.remotes.length) {
    list.innerHTML = `<div class="empty">No remotes yet. Add one, or import your existing rclone config from Settings.</div>`;
    return;
  }
  list.innerHTML = state.remotes
    .map(
      (r) => `<div class="item" data-name="${esc(r.name)}">
        <div class="info"><div class="title">${esc(r.name)}</div><div class="sub">${esc(r.kind)}</div></div>
        <div class="actions"><button class="danger" data-act="delete">Delete</button></div>
      </div>`,
    )
    .join("");
}

$("#remote-list").addEventListener("click", async (e) => {
  const btn = e.target.closest("button[data-act=delete]");
  if (!btn) return;
  const name = btn.closest(".item").dataset.name;
  if (!(await invoke("ask", { message: `Delete remote "${name}" from clonemount's rclone.conf? Files in the remote are not touched.` }))) return;
  await attempt(() => invoke("delete_remote", { name }));
  refresh();
  loadRemotesTab();
});

async function loadRemotesTab() {
  $("#config-cmd").textContent = await invoke("config_command");
  $("#conf-text").value = await invoke("get_rclone_conf");
}

$("#copy-cmd").addEventListener("click", async () => {
  await navigator.clipboard.writeText($("#config-cmd").textContent);
  toast("Copied");
});

$("#save-conf").addEventListener("click", async () => {
  await attempt(() => invoke("save_rclone_conf", { text: $("#conf-text").value }));
  toast("rclone.conf saved");
  refresh();
});

// ---------- remote dialog ----------

// rclone's `Provider` filter: comma-separated list, a leading "!" negates it.
function providerMatches(filter, provider) {
  if (!filter) return true;
  const negate = filter.startsWith("!");
  const names = filter.replace(/^!/, "").split(",");
  return names.includes(provider) !== negate;
}

function renderRemoteOptions() {
  const f = $("#remote-form");
  const backend = providers?.find((p) => p.Prefix === f.kind.value);
  const box = $("#remote-options");
  if (!backend) {
    box.innerHTML = "";
    return;
  }
  const advanced = $("#show-advanced").checked;
  const current = Object.fromEntries([...box.querySelectorAll("[data-opt]")].map((el) => [el.dataset.opt, el.value]));
  const provider = current.provider ?? "";
  const options = (backend.Options || []).filter(
    (o) => !o.Hide && (advanced || !o.Advanced || o.Required) && providerMatches(o.Provider, provider),
  );
  box.innerHTML = options
    .map((o) => {
      const help = (o.Help || "").split("\n")[0];
      const examples = (o.Examples || []).filter((x) => providerMatches(x.Provider, provider));
      const value = current[o.Name] ?? "";
      let field;
      if (examples.length && o.Exclusive) {
        field = `<select data-opt="${esc(o.Name)}"><option value="">${o.Required ? "Choose…" : "Default"}</option>${examples
          .map((x) => `<option value="${esc(x.Value)}" ${x.Value === value ? "selected" : ""}>${esc(x.Value)}${x.Help ? ` – ${esc(x.Help.split("\n")[0])}` : ""}</option>`)
          .join("")}</select>`;
      } else {
        const list = examples.length ? `list-${esc(o.Name)}` : "";
        field = `<input data-opt="${esc(o.Name)}" type="${o.IsPassword ? "password" : "text"}" value="${esc(value)}"
          placeholder="${esc(o.DefaultStr || "")}" ${list ? `list="${list}"` : ""} ${o.Required ? "required" : ""} />${
          list ? `<datalist id="${list}">${examples.map((x) => `<option value="${esc(x.Value)}">${esc(x.Help || "")}</option>`).join("")}</datalist>` : ""
        }`;
      }
      return `<label>${esc(o.Name)}${o.Required ? " *" : ""}<span class="opt-help">${esc(help)}</span>${field}</label>`;
    })
    .join("");
  // Some backends (S3) show different options per provider.
  box.querySelector('[data-opt="provider"]')?.addEventListener("change", renderRemoteOptions);
}

$("#add-remote").addEventListener("click", async () => {
  if (!providers) {
    providers = await attempt(() => invoke("providers"));
    providers.sort((a, b) => a.Description.localeCompare(b.Description));
  }
  const f = $("#remote-form");
  f.reset();
  f.kind.innerHTML =
    `<option value="">Choose…</option>` +
    providers.map((p) => `<option value="${esc(p.Prefix)}">${esc(p.Description)}</option>`).join("");
  $("#remote-options").innerHTML = "";
  $("#remote-error").textContent = "";
  $("#remote-dialog").showModal();
});

$("#remote-kind").addEventListener("change", () => {
  $("#remote-options").innerHTML = "";
  renderRemoteOptions();
});
$("#show-advanced").addEventListener("change", renderRemoteOptions);

$("#remote-form").addEventListener("submit", async (e) => {
  if (e.submitter?.value !== "save") return;
  e.preventDefault();
  const f = e.target;
  const options = Object.fromEntries(
    [...$("#remote-options").querySelectorAll("[data-opt]")].map((el) => [el.dataset.opt, el.value.trim()]),
  );
  const btn = $("#create-remote");
  btn.disabled = true;
  btn.textContent = "Creating…";
  try {
    await attempt(() => invoke("create_remote", { name: f.name.value, kind: f.kind.value, options }), $("#remote-error"));
    $("#remote-dialog").close();
    toast(`Remote "${f.name.value}" created`);
    refresh();
    loadRemotesTab();
  } finally {
    btn.disabled = false;
    btn.textContent = "Create";
  }
});

// ---------- settings ----------

function renderSettings(o) {
  $("#rclone-info").textContent = o.rclone_found
    ? `Using ${o.rclone_version || "rclone"} at ${o.rclone_found}`
    : "rclone was not found.";
  if (document.activeElement !== $("#rclone-path")) $("#rclone-path").value = o.rclone_path;
  $("#data-dir").textContent = o.data_dir;
  $("#portable-note").textContent = o.portable ? " (portable mode)" : "";
}

invoke("get_autostart").then((on) => ($("#autostart").checked = on));
$("#autostart").addEventListener("change", async (e) => {
  await attempt(() => invoke("set_autostart", { enabled: e.target.checked })).catch(() => (e.target.checked = !e.target.checked));
});

$("#save-rclone").addEventListener("click", async () => {
  await attempt(() => invoke("set_rclone_path", { path: $("#rclone-path").value }));
  toast("Saved");
  refresh();
});

$("#export").addEventListener("click", async () => {
  if (await attempt(() => invoke("export_config"))) toast("Exported");
});

$("#import").addEventListener("click", async () => {
  if (!(await invoke("ask", { message: "Importing replaces your current drives and remotes. Continue?" }))) return;
  if (await attempt(() => invoke("import_config"))) {
    toast("Imported");
    refresh();
  }
});

$("#open-data").addEventListener("click", () => attempt(() => invoke("open_data_dir")));

refresh();
