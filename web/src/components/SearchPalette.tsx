import { CornerDownLeft, Languages, Search, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { useSearch } from "../api/hooks";
import { languageName, plural, sourceName } from "../lib/format";
import { useApp } from "../live/store";

const EXAMPLES = ["huelga de estudiantes en Francia", "Flugzeug Notlandung", "election results", "ceasefire talks"];

/** ⌘K: semantic search across every language, grouped by story. */
export function SearchPalette() {
  const open = useApp((s) => s.searchOpen);
  const setOpen = useApp((s) => s.setSearchOpen);
  const select = useApp((s) => s.select);
  const goLive = useApp((s) => s.goLive);
  const [input, setInput] = useState("");
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const search = useSearch(query);

  useEffect(() => {
    const t = window.setTimeout(() => setQuery(input), 280);
    return () => window.clearTimeout(t);
  }, [input]);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);

  const results = search.data?.results ?? [];
  const activeIndex = Math.min(active, Math.max(0, results.length - 1));
  const choose = (id: string) => {
    goLive(); // search reflects the current state
    select(id);
    setOpen(false);
  };

  return (
    <dialog
      ref={dialogRef}
      className="palette"
      onClose={() => setOpen(false)}
      onClick={(e) => e.target === dialogRef.current && setOpen(false)}
      aria-label="Search stories"
    >
      <div className="palette__input">
        <Search size={18} aria-hidden />
        <input
          autoFocus
          value={input}
          onChange={(e) => {
            setInput(e.target.value);
            setActive(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") setActive((a) => Math.min(results.length - 1, a + 1));
            else if (e.key === "ArrowUp") setActive((a) => Math.max(0, a - 1));
            else if (e.key === "Enter" && results[activeIndex]) choose(results[activeIndex]!.story.id);
            else return;
            e.preventDefault();
          }}
          placeholder="Search stories in any language…"
          aria-label="Search query"
          aria-controls="palette-results"
          aria-activedescendant={results[activeIndex] ? `result-${active}` : undefined}
        />
        {search.isFetching && <span className="palette__spinner" aria-label="Searching" />}
        <button className="icon-btn" onClick={() => setOpen(false)} aria-label="Close search">
          <X size={16} />
        </button>
      </div>

      {query.trim().length < 2 ? (
        <div className="palette__intro">
          <p>
            <Languages size={15} aria-hidden /> Search by meaning, across languages. A Spanish query finds English coverage.
          </p>
          <ul>
            {EXAMPLES.map((ex) => (
              <li key={ex}>
                <button
                  className="chip"
                  onClick={() => {
                    setInput(ex);
                    setActive(0);
                  }}
                >
                  {ex}
                </button>
              </li>
            ))}
          </ul>
        </div>
      ) : search.isError ? (
        <p className="palette__empty">Search needs the embedding service (make embedder).</p>
      ) : results.length === 0 && !search.isFetching ? (
        <p className="palette__empty">No stories match.</p>
      ) : (
        <ul id="palette-results" className="palette__results" role="listbox">
          {results.slice(0, 8).map((r, i) => (
            <li key={r.story.id} id={`result-${i}`} role="option" aria-selected={i === activeIndex}>
              <button
                className={`result ${i === activeIndex ? "is-active" : ""}`}
                onMouseEnter={() => setActive(i)}
                onClick={() => choose(r.story.id)}
              >
                <span className="result__top">
                  <span className={`match ${r.strong ? "match--strong" : ""}`}>{r.strong ? "Close match" : "Related"}</span>
                  <span className="muted num">
                    {plural(r.story.source_count, "source")} · {r.story.langs.length} langs
                  </span>
                </span>
                <span className="result__headline">{r.story.headline}</span>
                <span className="result__hits">
                  {r.hits.slice(0, 2).map((h) => (
                    <span key={h.article.id} className="result__hit">
                      <span className="lang" title={languageName(h.article.lang)}>
                        {h.article.lang}
                      </span>
                      <span className="result__hit-title">{h.article.title}</span>
                      <span className="muted">{sourceName(h.article.source_id)}</span>
                    </span>
                  ))}
                </span>
                {i === activeIndex && <CornerDownLeft size={14} className="result__enter" aria-hidden />}
              </button>
            </li>
          ))}
        </ul>
      )}
    </dialog>
  );
}
