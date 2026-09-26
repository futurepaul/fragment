// The computer runtime: jobs that run commands on the fragment's own
// computer (`job.computer.exec`).
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  async shell({ command, opts }, job) {
    return job.computer.exec(command, opts ?? {});
  }

  // Counts its runs in a file on the computer, then fails its first
  // attempt: a replay reattaches to the command, which does not run again.
  async count(input, job) {
    const r = await job.computer.exec('n=$(cat "$HOME/count" 2> /dev/null || echo 0); echo $((n + 1)) | tee "$HOME/count"');
    if (job.attempt === 1) throw new Error("held once, to be replayed");
    return r;
  }
}
