// A doc: a page of writing that its readers leave notes on, paragraph by
// paragraph, as a document's comments. The notes are applib/notes.mjs, a
// mixin this App takes whole: a fragment of your own adds them the same
// way (README.md). Operations of the doc's own go in this class beside
// them, under names of their own.
import { DurableObject } from "cloudflare:workers";
import { withNotes } from "./applib/notes.mjs";

export class App extends withNotes(DurableObject) {}
