import { share, feedback } from "./response-actions.js";

const IMAGE = /^image\/(png|jpeg|webp|gif)$/;
const AUDIO = /^audio\/(webm|ogg|mp4|mpeg|wav)$/;
const VIDEO = /^video\/(mp4|webm)$/;
const essence = (type) => String(type).split(";")[0].trim().toLowerCase();

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

function link(href, label, cls) {
  const node = el("a", cls, label);
  node.href = href;
  node.target = "_blank";
  node.rel = "noopener noreferrer";
  return node;
}

// Only passive types the cell serves inline are embedded. All attachments
// retain open/download actions, including when a browser cannot show PDFs.
export function renderAttachments(list) {
  const files = el("div", "message-attachments");
  for (const attachment of list) {
    const href = `./__blob/${attachment.sha256}`;
    const name = attachment.name || "Attachment";
    const type = essence(attachment.type);
    const item = el("div", "attachment");
    if (IMAGE.test(type)) {
      const preview = link(href, "", "attachment-image");
      const img = el("img");
      Object.assign(img, { src: href, alt: name, loading: "lazy" });
      preview.append(img);
      item.append(preview);
    } else if (AUDIO.test(type) || VIDEO.test(type)) {
      const video = VIDEO.test(type);
      const preview = el("div", video ? "attachment-video" : "attachment-audio");
      const player = el(video ? "video" : "audio");
      Object.assign(player, { src: href, controls: true, preload: "metadata" });
      if (video) player.playsInline = true;
      player.setAttribute("aria-label", name);
      preview.append(player);
      item.append(preview);
    } else if (type === "application/pdf") {
      const preview = el("iframe", "attachment-pdf");
      preview.src = `${href}#page=1`;
      preview.title = name;
      preview.loading = "lazy";
      item.append(preview);
    }
    const actions = el("div", "attachment-actions");
    actions.append(link(href, name, "attachment-name"));
    const download = link(href, "Download", "attachment-download");
    download.download = attachment.name || attachment.sha256.slice(0, 12);
    download.setAttribute("aria-label", `Download ${name}`);
    const button = el("button", "attachment-share", "Share");
    button.type = "button";
    button.setAttribute("aria-label", `Share ${name}`);
    button.onclick = () => feedback(button, () => share({ title: name, url: new URL(href, location.href).href }));
    actions.append(download, button);
    item.append(actions);
    files.append(item);
  }
  return files;
}
