Group the hunks of a code review by functional capability, so that a reviewer can understand a large change without reading it file by file.

# Task

- Put hunks that implement the same functional capability in the same group: a feature, a use case, a business concept, or one cross-cutting technical concern (build, logging, a refactoring...). A group should be readable in one sitting: typically 3 to 40 hunks. Split bigger capabilities into sub-capabilities. Small unrelated leftovers may share one "Miscellaneous" group.
- Inside each group, split the hunks into technical layers. Use these names: model, persistence, config, build, events, service, api, ui, cli, docs, tests; use core for code that fits none of them. Invent another short lowercase name only when the project clearly has such a layer. A layer is never empty. A group may have a single layer.
- Give each group a title of at most 8 words naming the capability (not the files), and a summary of 2 to 3 short sentences explaining what the change does functionally: which behaviour is added, changed or removed, and what for. Do not describe the code line by line.
- Write every title and summary in {{language}}. Everything else stays exactly as specified: the JSON keys, the layer names above (in English) and the numeric hunk ids.
- List the groups in reading order: foundations first (models, contracts, configuration), then what uses them, then entry points, tests last.

# Rules

- There are {{count}} hunks. Every hunk id listed below must appear in exactly one layer of exactly one group: no unknown id, no repeated id, no missing id.
- Use only the numeric ids from the input. Never quote or reproduce code: refer to functions, classes or files by name when needed.
- Before answering, check that no id is missing: forgotten ids are the most common mistake.
- Answer with the JSON object only: no prose, no markdown fence.

# Output format

{"groups":[{"title":"...","summary":"...","layers":[{"name":"service","hunks":[3,4]},{"name":"tests","hunks":[9]}]}]}
{{instructions}}
# Changes

Each `file` line gives a path, its status and language. Each following line is one hunk of that file: `[id] @@ enclosing section @@ +added -removed | first changed lines`.

{{hunks}}
