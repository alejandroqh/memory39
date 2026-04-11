"""Memory39 adapter for the Agent Memory Benchmark (OMB).

Wraps the memory39 CLI binary as an OMB MemoryProvider. Each benchmark run
gets a fresh SQLite database so results are isolated.
"""

import os
import subprocess
from pathlib import Path

from memory_bench.memory.base import MemoryProvider
from memory_bench.models import Document


class Memory39MemoryProvider(MemoryProvider):
    name = "memory39"
    description = "Temporal-priority memory with LLM-driven extraction, SQLite FTS5 retrieval."
    kind = "local"
    concurrency = 1  # sequential — each ingest spawns an LLM agent loop

    def __init__(self):
        self._binary = os.environ.get("MEMORY39_BIN", "memory39")
        self._llm = os.environ.get("MEMORY39_LLM", "deepseek")
        self._model = os.environ.get("MEMORY39_MODEL") or None
        self._db_path: str | None = None

    def prepare(self, store_dir: Path, unit_ids: set[str] | None = None, reset: bool = True) -> None:
        db = store_dir / "memory39.db"
        self._db_path = str(db)
        if reset:
            db.unlink(missing_ok=True)

    def _run(self, args: list[str], input_text: str | None = None) -> subprocess.CompletedProcess:
        cmd = [self._binary, "--db", self._db_path, "--llm", self._llm]
        if self._model:
            cmd += ["--model", self._model]
        cmd += args
        return subprocess.run(cmd, capture_output=True, text=True, input=input_text, timeout=300)

    def ingest(self, documents: list[Document]) -> None:
        for doc in documents:
            text = doc.content
            if not text or not text.strip():
                continue
            # Pass conversation text via stdin
            result = self._run(["ingest", "-"], input_text=text)
            if result.returncode != 0:
                print(f"[memory39] ingest error for doc {doc.id}: {result.stderr[:200]}")

    def retrieve(self, query: str, k: int = 10, user_id: str | None = None,
                 query_timestamp: str | None = None) -> tuple[list[Document], dict | None]:
        args = ["recall", query, "--limit", str(k)]
        result = self._run(args)
        if result.returncode != 0:
            return [], None

        # Parse recall output into Documents
        docs = []
        current_id = None
        current_lines = []

        for line in result.stdout.splitlines():
            if line.startswith("[") and "]" in line:
                # Save previous
                if current_id and current_lines:
                    docs.append(Document(id=current_id, content="\n".join(current_lines), user_id=user_id))
                # Parse new: [E3] event (dated) (score: 0.73)
                bracket_end = line.index("]")
                current_id = line[1:bracket_end]
                current_lines = [line]
            elif current_id:
                current_lines.append(line)

        if current_id and current_lines:
            docs.append(Document(id=current_id, content="\n".join(current_lines), user_id=user_id))

        return docs[:k], None
