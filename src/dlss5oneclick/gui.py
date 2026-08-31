"""PySide6 window: pick game exe, see what's present, one Install button."""
from __future__ import annotations

import logging
import sys
from pathlib import Path

from PySide6.QtCore import QObject, QSettings, Qt, QThread, Signal
from PySide6.QtWidgets import (
    QApplication, QFileDialog, QHBoxLayout, QLabel, QLineEdit, QMessageBox,
    QPlainTextEdit, QProgressBar, QPushButton, QVBoxLayout, QWidget,
)

from dlss5oneclick import __version__, game, installer

logger = logging.getLogger(__name__)

ROWS = [
    ("reshade", "ReShade (add-on build) as dxgi.dll"),
    ("feeder", "DLSS5-Feeder add-on + DLSS5_Feed.fx"),
    ("lumenite", "LumeniteFX motion vectors"),
    ("dlss5_addon", "renodx-dlss5.addon64"),
    ("dlssnr", "nvngx_dlssnr.dll (DLSS 5 model)"),
    ("dlss", "nvngx_dlss.dll"),
]


class Worker(QObject):
    progress = Signal(int, str)
    step = Signal(int, str, str, str)
    finished = Signal(bool, str)

    def __init__(self, exe: Path, do_uninstall: bool = False):
        super().__init__()
        self._exe = exe
        self._uninstall = do_uninstall

    def run(self) -> None:
        try:
            if self._uninstall:
                removed = installer.uninstall(self._exe)
                self.finished.emit(True, "Removed:\n" + "\n".join(removed or ["nothing"]))
                return
            results = installer.run_all(
                self._exe,
                progress=lambda p, m: self.progress.emit(p, m),
                step_cb=lambda i, n, s, d: self.step.emit(i, n, s, d),
            )
            lines = [f"{k}: {', '.join(v) if v else 'already present'}"
                     for k, v in results.items()]
            self.finished.emit(True, "\n".join(lines))
        except installer.InstallError as e:
            self.finished.emit(False, str(e))
        except Exception as e:  # noqa: BLE001 — surface, never hang the UI
            logger.exception("install failed")
            self.finished.emit(False, f"Unexpected error: {e}")


class MainWindow(QWidget):
    def __init__(self):
        super().__init__()
        self.setWindowTitle(f"DLSS5oneclick {__version__}")
        self.setMinimumWidth(640)
        self._settings = QSettings("DLSS5oneclick", "DLSS5oneclick")
        self._thread: QThread | None = None
        self._worker: Worker | None = None

        root = QVBoxLayout(self)

        pick = QHBoxLayout()
        self._exe_edit = QLineEdit(self._settings.value("exe", "", str))
        self._exe_edit.setPlaceholderText("Game executable (the .exe you launch)")
        browse = QPushButton("Browse…")
        browse.clicked.connect(self._browse)
        pick.addWidget(self._exe_edit, 1)
        pick.addWidget(browse)
        root.addLayout(pick)

        self._row_labels: dict[str, QLabel] = {}
        for key, text in ROWS:
            lab = QLabel(f"○  {text}")
            self._row_labels[key] = lab
            root.addWidget(lab)
        self._problem = QLabel("")
        self._problem.setStyleSheet("color:#c0392b")
        self._problem.setWordWrap(True)
        root.addWidget(self._problem)

        btns = QHBoxLayout()
        self._install = QPushButton("Install DLSS 5")
        self._install.setDefault(True)
        self._install.clicked.connect(lambda: self._start(False))
        self._remove = QPushButton("Remove")
        self._remove.clicked.connect(lambda: self._start(True))
        btns.addWidget(self._install, 1)
        btns.addWidget(self._remove)
        root.addLayout(btns)

        self._bar = QProgressBar()
        self._bar.setRange(0, 100)
        root.addWidget(self._bar)
        self._status = QLabel("")
        root.addWidget(self._status)

        self._log = QPlainTextEdit()
        self._log.setReadOnly(True)
        self._log.setMaximumBlockCount(500)
        root.addWidget(self._log, 1)

        after = QLabel(
            "After install, in game: press Home for the ReShade overlay, open the "
            "DLSS 5 Neural Rendering panel and enable it. Keep MSAA/SSAA off. "
            "Check dlss5-feed.log next to the exe for 'feature ready'.")
        after.setWordWrap(True)
        root.addWidget(after)

        self._exe_edit.textChanged.connect(self._refresh)
        self._refresh()

    # ── helpers ──────────────────────────────────────────────────────
    def _browse(self) -> None:
        start = str(Path(self._exe_edit.text()).parent) if self._exe_edit.text() else ""
        path, _ = QFileDialog.getOpenFileName(
            self, "Pick the game executable", start, "Executables (*.exe)")
        if path:
            self._exe_edit.setText(path)

    def _exe(self) -> Path | None:
        t = self._exe_edit.text().strip()
        return Path(t) if t else None

    def _refresh(self) -> None:
        exe = self._exe()
        self._problem.setText("")
        if not exe or not exe.is_file():
            for lab in self._row_labels.values():
                lab.setText("○  " + lab.text()[3:])
            self._install.setEnabled(False)
            self._remove.setEnabled(False)
            return
        try:
            st = game.inspect(exe)
        except game.GameError as e:
            self._problem.setText(str(e))
            self._install.setEnabled(False)
            self._remove.setEnabled(False)
            return
        self._settings.setValue("exe", str(exe))
        for key, text in ROWS:
            ok = getattr(st, key)
            self._row_labels[key].setText(("✔  " if ok else "○  ") + text)
        self._problem.setText("\n".join(st.problems))
        self._install.setEnabled(not st.problems)
        self._remove.setEnabled(True)
        self._status.setText("Everything is in place." if st.complete else "")

    def _start(self, do_uninstall: bool) -> None:
        exe = self._exe()
        if not exe:
            return
        if do_uninstall and QMessageBox.question(
                self, "Remove DLSS 5 files",
                "Remove DLSS5-Feeder, LumeniteFX and the DLSS 5 add-on from this game?\n"
                "ReShade itself and nvngx_dlss.dll stay.") != QMessageBox.StandardButton.Yes:
            return
        self._install.setEnabled(False)
        self._remove.setEnabled(False)
        self._log.clear()
        self._bar.setValue(0)
        self._thread = QThread(self)
        self._worker = Worker(exe, do_uninstall)
        self._worker.moveToThread(self._thread)
        self._thread.started.connect(self._worker.run)
        self._worker.progress.connect(self._on_progress)
        self._worker.step.connect(self._on_step)
        self._worker.finished.connect(self._on_finished)
        self._worker.finished.connect(self._thread.quit)
        self._thread.start()

    def _on_progress(self, pct: int, msg: str) -> None:
        self._bar.setValue(pct)
        self._status.setText(msg)

    def _on_step(self, i: int, name: str, state: str, detail: str) -> None:
        n = len(installer.STEPS)
        if state == "start":
            self._log.appendPlainText(f"[{i + 1}/{n}] {name}…")
        elif state == "done":
            self._log.appendPlainText(f"      ✔ {detail}")
        else:
            self._log.appendPlainText(f"      ✖ {detail}")

    def _on_finished(self, ok: bool, message: str) -> None:
        self._bar.setValue(100 if ok else self._bar.value())
        self._status.setText("Done." if ok else "Failed.")
        self._log.appendPlainText(message)
        self._refresh()
        if not ok:
            QMessageBox.critical(self, "DLSS5oneclick", message)


def main() -> int:
    logging.basicConfig(level=logging.INFO)
    app = QApplication(sys.argv)
    app.setApplicationName("DLSS5oneclick")
    w = MainWindow()
    w.show()
    return app.exec()
