const status = document.querySelector("#status");
const button = document.querySelector("#fill");
const appliedButton = document.querySelector("#applied");
let loaded = null;

button.addEventListener("click", () => {
  button.disabled = true;
  status.textContent = "Reading the open page…";
  fillActiveTab().then(
    (message) => {
      status.textContent = message;
      button.disabled = false;
    },
    (error) => {
      status.textContent = error instanceof Error ? error.message : "Could not fill this page";
      button.disabled = false;
    },
  );
});

loadProfile();

appliedButton.addEventListener("click", () => {
  appliedButton.disabled = true;
  status.textContent = "Reporting to the agent…";
  reportApplied().then(
    (message) => {
      status.textContent = message;
      appliedButton.disabled = false;
    },
    (error) => {
      status.textContent = error instanceof Error ? error.message : "Could not report";
      appliedButton.disabled = false;
    },
  );
});

async function reportApplied() {
  const tab = await activePage();
  const response = await fetch("http://127.0.0.1:8787/api/applications/report-applied", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ url: tab.url }),
  });
  if (!response.ok) {
    throw new Error((await response.text()) || "The agent refused the report.");
  }
  return "Marked applied in the agent. Interview, offer, and rejection updates arrive via email sync.";
}

async function loadProfile() {
  status.textContent = "Reading your saved resume…";
  try {
    const tab = await activePage();
    loaded = await agentProfile(tab.url);
    const companies = (loaded.companies || []).join(", ") || "none stored";
    const schools = (loaded.schools || []).join(", ") || "none stored";
    status.textContent = `${loaded.full_name || "No name saved"}\n${loaded.email || "No email saved"}\nCompanies: ${companies}\nSchools: ${schools}`;
  } catch (error) {
    button.disabled = true;
    status.textContent = error instanceof Error ? error.message : "Could not read the agent";
  }
}

async function activePage() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id || !tab.url || !/^https?:\/\//.test(tab.url)) {
    throw new Error("Open the application page, then open this popup again.");
  }
  return tab;
}

async function agentProfile(pageUrl) {
  let response;
  try {
    response = await fetch(`http://127.0.0.1:8787/api/autofill?url=${encodeURIComponent(pageUrl)}`);
  } catch {
    throw new Error("The agent is not running. Restart .\\dev.cmd, then reload this extension.");
  }
  if (!response.ok) {
    throw new Error((await response.text()) || "The agent refused the autofill request.");
  }
  return response.json();
}

async function fillActiveTab() {
  const tab = await activePage();
  const profile = loaded || (await agentProfile(tab.url));
  const file = await agentResumeFile();
  const [injected] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: fillForm,
    args: [profile, file],
  });
  const result = injected?.result ?? { filled: 0, files: 0, notes: [], attached: [] };
  const notes = (result.notes || []).join("\n");
  const attached = (result.attached || []).join(", ");
  const summary = result.filled
    ? `Typed ${result.filled} empty field${result.filled === 1 ? "" : "s"}. Pick each company or school from the site's list. The extension does not press Save.`
    : "No empty field matched. Company and school boxes that say “start typing” need a choice from the site's list, and a box that already has text is left alone.";
  const upload = result.files
    ? (attached
      ? `Resume attached to: ${attached}.`
      : "This page has a resume upload, but attaching failed — attach it yourself.")
    : "";
  return [summary, upload, notes].filter(Boolean).join("\n");
}

// Resume bytes (base64) for the page. Null when no resume is uploaded.
async function agentResumeFile() {
  try {
    const response = await fetch("http://127.0.0.1:8787/api/resume/file");
    if (!response.ok) return null;
    const disposition = response.headers.get("content-disposition") || "";
    const name = (/filename="([^"]+)"/.exec(disposition) || [])[1] || "resume.pdf";
    const buffer = await response.arrayBuffer();
    const bytes = new Uint8Array(buffer);
    let binary = "";
    for (let i = 0; i < bytes.length; i += 0x8000) {
      binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    }
    return { name, mime: response.headers.get("content-type") || "application/pdf", base64: btoa(binary) };
  } catch {
    return null;
  }
}

function fillForm(profile, file) {
  const denied = [
    "gender", "race", "ethnic", "veteran", "disability", "disabled", "hispanic", "latino",
    "pronoun", "religion", "citizen", "sponsor", "visa", "authorized", "authorised",
    "equal opportunity", "eeoc", "demographic", "felony", "criminal", "salary",
    "compensation", "linkedin", "github", "website", "portfolio", "password",
  ];
  const fields = [...document.querySelectorAll("input, textarea, [contenteditable='true']")].filter((field) => {
    if (field.disabled || field.readOnly) return false;
    const type = (field.getAttribute("type") || "text").toLowerCase();
    return !["hidden", "submit", "button", "checkbox", "radio", "file", "password", "image", "reset"].includes(type);
  });
  const files = document.querySelectorAll('input[type="file"]').length;
  const attached = [];
  if (file && file.base64) {
    const bytes = base64ToBytes(file.base64);
    if (bytes) {
      for (const input of document.querySelectorAll('input[type="file"]')) {
        if (input.disabled) continue;
        try {
          const upload = new File([bytes], file.name || "resume.pdf", { type: file.mime || "application/pdf" });
          const transfer = new DataTransfer();
          transfer.items.add(upload);
          input.files = transfer.files;
          input.dispatchEvent(new Event("input", { bubbles: true }));
          input.dispatchEvent(new Event("change", { bubbles: true }));
          attached.push(fieldLabel(input) || input.name || "resume upload");
        } catch {
          // Some custom upload widgets reject programmatic files; the user attaches manually.
        }
      }
    }
  }
  const pageText = document.body ? document.body.innerText : "";
  const notes = [];
  let filled = 0;
  for (const field of fields) {
    const existing = field.isContentEditable ? field.textContent : field.value;
    if (String(existing || "").trim()) continue;
    const label = fieldLabel(field);
    if (!label || denied.some((word) => label.includes(word))) continue;
    const value = valueFor(field, label, profile, pageText, notes);
    if (!value) continue;
    setValue(field, value);
    filled += 1;
  }
  for (const select of document.querySelectorAll("select")) {
    if (select.disabled || String(select.value || "").trim()) continue;
    const label = fieldLabel(select);
    if (!label || !profile.country || denied.some((word) => label.includes(word))) continue;
    if (!/\bcountry\b/.test(label)) continue;
    const wanted = profile.country.trim().toLowerCase();
    const option = [...select.options].find((item) => {
      const text = item.text.trim().toLowerCase();
      const value = item.value.trim().toLowerCase();
      return text === wanted || value === wanted;
    });
    if (!option) continue;
    select.value = option.value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
    filled += 1;
  }
  return { filled, files, attached, notes };

  function base64ToBytes(base64) {
    try {
      const binary = atob(base64);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
      return bytes;
    } catch {
      return null;
    }
  }

  function fieldLabel(field) {
    const bits = [field.name, field.id, field.placeholder, field.getAttribute("aria-label"), field.getAttribute("autocomplete")];
    if (field.id) {
      const label = document.querySelector(`label[for="${CSS.escape(field.id)}"]`);
      if (label) bits.push(label.innerText);
    }
    const wrapping = field.closest("label");
    if (wrapping) bits.push(wrapping.innerText);
    bits.push(nearbyHeading(field));
    return bits.filter(Boolean).join(" ").toLowerCase();
  }

  function nearbyHeading(field) {
    let node = field.parentElement;
    for (let depth = 0; depth < 5 && node; depth += 1) {
      for (const child of node.children) {
        if (child === field || child.contains(field)) continue;
        if (!/^(H1|H2|H3|H4|STRONG|LEGEND)$/.test(child.tagName)) continue;
        const text = child.innerText.trim();
        if (text && text.length < 80) return text;
      }
      node = node.parentElement;
    }
    return "";
  }

  function valueFor(field, label, profile, pageText, notes) {
    const long = field.tagName === "TEXTAREA";
    if (/\be-?mail\b/.test(label)) return profile.email;
    if (/\b(phone|mobile|tel)\b/.test(label)) return profile.phone;
    if (/\bfirst\b/.test(label) && /\bname\b/.test(label)) return profile.first_name;
    if (/\b(last|surname|family)\b/.test(label) && /\bname\b/.test(label)) return profile.last_name;
    if (/\bfull name\b|\byour name\b|\bcandidate name\b|\blegal name\b/.test(label)) return profile.full_name;
    if (/\bname\b/.test(label) && !/\b(first|last|user|company|job|file)\b/.test(label)) return profile.full_name;
    if (/\bcity\b|\blocation\b/.test(label)) return profile.location;
    if (/\bcountry\b/.test(label)) return profile.country;
    if (/\b(company|companies|employer|organisation|organization)\b/.test(label)) {
      return nextMissing(profile.companies, pageText, notes, "Every saved company is already on the page.");
    }
    if (/\b(college|colleges|school|schools|institute|university|education)\b/.test(label)) {
      return nextMissing(profile.schools, pageText, notes, "Every saved school is already on the page.");
    }
    if (/\b(current title|job title)\b/.test(label)) return profile.current_title;
    if (/\bskills?\b/.test(label)) return (profile.skills || []).join(", ");
    if (long && /\b(achievement|cover letter|why do you|additional information)\b/.test(label)) {
      return profile.cover_letter || profile.summary || profile.headline;
    }
    if (long && /\b(summary|about you)\b/.test(label)) return profile.summary || profile.headline;
    return "";
  }

  function nextMissing(values, pageText, notes, doneNote) {
    const found = (values || []).find((value) => value && !pageText.toLowerCase().includes(value.toLowerCase()));
    if (!found && values?.length && !notes.includes(doneNote)) notes.push(doneNote);
    return found || "";
  }

  function setValue(field, value) {
    if (field.isContentEditable) {
      field.textContent = value;
      field.dispatchEvent(new InputEvent("input", { bubbles: true, data: value }));
      return;
    }
    const prototype = field instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set;
    if (setter) setter.call(field, value);
    else field.value = value;
    field.dispatchEvent(new Event("input", { bubbles: true }));
    field.dispatchEvent(new Event("change", { bubbles: true }));
  }
}
