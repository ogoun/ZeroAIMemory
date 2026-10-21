# Makefile: .PHONY targets

Symptom: `make docs` printed `make: 'docs' is up to date.` and did nothing. The repo has a
`docs/` directory, so make treated the target as a file that is already present and newer
than its (zero) prerequisites.

Declare task-style targets as phony:

```make
.PHONY: docs test lint clean
docs:
	mkdocs build
test:
	pytest -q
clean:
	rm -rf dist .pytest_cache
```

Effects of `.PHONY`:

- the target always runs, regardless of any file on disk with that name;
- make skips implicit rule lookup for it, a tiny speedup in big makefiles;
- a phony target should not be a prerequisite of a real file target, or that file is rebuilt
  every time.

Other reminders: command lines start with a TAB, not spaces (`missing separator. Stop.`);
each line runs in its own shell, so `cd` does not carry over to the next line unless you
join them with `&&` or use `.ONESHELL:`.
