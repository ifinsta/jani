# Claims register

This folder is the durable documentation discrepancy register for Jani. It is not an implementation authority or a permission source.

- `index.json`: stable machine-readable source/document audit findings.
- `OPEN.md`: human-readable open findings.
- `agent-reviews/*.json`: untrusted AI reviewer hypotheses with independently checked source citations.
- `review-queue.json` (optional): generated candidate statements for an authorized Jani reviewer.

Use `cargo run -p jani-claims -- sync` at repository root to regenerate the first two files. CI runs `cargo run -p jani-claims -- check` to detect stale reports. Review and correct source or Markdown claims deliberately; never auto-delete findings by editing generated JSON.

## Agent review input contract

An agent may propose suspected semantic contradictions in a JSON file here:

```json
{
  "candidates": [
    {
      "document": "docs/ARCHITECTURE.md",
      "line": 21,
      "statement": "Exact excerpt from that Markdown line",
      "sourcePath": "backend/server/src/main.rs",
      "sourceQuote": "An exact snippet present in that source file",
      "explanation": "Why the quoted evidence appears to contradict the documented statement."
    }
  ]
}
```

The scanner validates both citations against existing files and **never** promotes the model's verdict to confirmed. Each remains `needs_human_review` even if the quote exists; a quote may lack context. Each file supports at most 100 candidates and a 128 KiB input limit.

### Triage

Investigate the cited source, update inaccurate documents or implementation, run conformance tests where applicable, then regenerate the register. Keep proposals separate from implemented-feature claims. For a critical security claim, validation requires independent tests and a human reviewer, not simply keyword presence.
