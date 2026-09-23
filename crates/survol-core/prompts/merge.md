The hunks of a large code review were grouped module by module, because the whole review does not fit in one request. Groups from different modules may describe the same functional capability. Decide which groups to merge.

# Rules

- Merge only groups that clearly implement the same capability, for example the backend and frontend parts of one feature, or the pieces of one use case spread over several modules. When in doubt, do not merge.
- A merge lists at least two group ids. Each group id appears in at most one merge. Groups you do not mention stay as they are.
- For each merge, give a title of at most 8 words naming the capability, and a summary of 2 to 3 sentences explaining what the merged change does functionally.
- Write every title and summary in {{language}}. The JSON keys stay in English.
- Use only the ids below. Never quote code. Answer with the JSON object only: no prose, no markdown fence.

# Output format

{"merges":[{"groups":[0,5],"title":"...","summary":"..."}]}

Answer {"merges":[]} when nothing should be merged.
{{instructions}}
# Groups

Each line: `[id] title (directories; layers with hunk counts) — summary`.

{{groups}}
