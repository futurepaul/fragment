// Files as an app's state (slice E): mutations write notes to main, queries
// read them through this.files, jobs read and compare-and-swap as steps, and
// a file trigger's write starts the next run one hop deeper.
import { DurableObject } from "cloudflare:workers";

const POINTER = `version https://git-lfs.github.com/spec/v1\noid sha256:${"a".repeat(64)}\nsize 5\n`;

export class App extends DurableObject {
  add_note({ slug, text }, call) {
    call.files.write(`notes/${slug}.md`, text);
    call.publish("changes", { slug }, "note");
    return { path: `notes/${slug}.md` };
  }

  drop_note({ slug }, call) {
    call.files.remove(`notes/${slug}.md`);
    return { ok: true };
  }

  // two files in one mutation: one commit
  pair({ a, b }, call) {
    call.files.write("pair/a.txt", a);
    call.files.write("pair/b.bin", new Uint8Array([0, 159, 146, 150]));
    return { ok: true };
  }

  bad_path(_input, call) {
    call.files.write("../escape.txt", "no");
  }

  async read({ path }) {
    return { text: await this.files.read(path) };
  }

  // how long a file read into the app is (a large one's text would not
  // fit a result)
  async measure({ path }) {
    return { length: (await this.files.read(path))?.length ?? null };
  }

  async bytes({ path }) {
    const data = await this.files.readBytes(path);
    return { bytes: data === null ? null : [...data] };
  }

  async list({ prefix }) {
    return await this.files.list(prefix);
  }

  async stat({ path }) {
    return await this.files.stat(path);
  }

  // appends a line to log.txt, compare-and-swapped on what it read
  async append({ line }, job) {
    const before = await job.files.stat("log.txt");
    const text = (await job.files.read("log.txt")) ?? "";
    const wrote = await job.files.write("log.txt", `${text}${line}\n`, { expect: before ? before.sha : null });
    return { commit: wrote.commit, lines: `${text}${line}\n`.split("\n").length - 1 };
  }

  // a write that names the wrong blob fails its step, and the job may catch it
  async stale({ expect }, job) {
    try {
      await job.files.write("log.txt", "clobbered\n", { expect });
      return { conflict: null };
    } catch (e) {
      return { conflict: e.message, name: e.name };
    }
  }

  // a pointer written by a step past an in-app check the author's code
  // broke: the cell refuses the step for good, and the job may catch that
  async sneaky(_input, job) {
    const decode = TextDecoder.prototype.decode;
    TextDecoder.prototype.decode = () => {
      throw new TypeError("patched");
    };
    let wrote;
    try {
      wrote = job.files.write("sneaky.bin", POINTER);
    } finally {
      TextDecoder.prototype.decode = decode;
    }
    try {
      await wrote;
      return { refused: null };
    } catch (e) {
      return { refused: e.message, name: e.name };
    }
  }

  // the file trigger on loop/: the run for loop/0.txt writes loop/1.txt,
  // and the run that write starts writes nothing
  async again({ paths }, job) {
    const n = Number(paths[0].match(/(\d+)/)?.[1] ?? 0) + 1;
    if (n === 1) await job.files.write(`loop/${n}.txt`, String(n));
    return { n };
  }

  async whoami() {
    return { text: await this.files.read("whoami.txt") };
  }
}
