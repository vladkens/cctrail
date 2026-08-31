import os
import tomllib
from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path


class StatsBy(StrEnum):
    PROJECT = "project"
    GROUP = "group"
    WORKDIR = "workdir"


@dataclass
class Project:
    group: str
    name: str
    roots: list[Path]


@dataclass
class Config:
    projects: list[Project] = field(default_factory=list)
    _cache: dict[str, Project | None] = field(
        default_factory=dict,
        init=False,
        repr=False,
        compare=False,
    )

    @classmethod
    def load(cls) -> "Config":
        home = home_dir()
        path = config_path(home)
        if "CCTRAIL_CONFIG" in os.environ and path:
            return cls.read(path, home)

        return cls.read(path, home) if path and path.is_file() else cls()

    @classmethod
    def read(cls, path: Path, home: Path | None = None) -> "Config":
        try:
            text = path.read_text()
        except OSError as e:
            raise ValueError(f"failed to read {path}: {e}") from e
        try:
            return cls.parse(text, home)
        except (ValueError, tomllib.TOMLDecodeError) as e:
            raise ValueError(f"failed to parse {path}: {e}") from e

    @classmethod
    def parse(cls, text: str, home: Path | None = None) -> "Config":
        data = tomllib.loads(text)
        if set(data) - {"projects"}:
            key = next(iter(set(data) - {"projects"}))
            raise ValueError(f"unknown field `{key}`")
        raw = data.get("projects", {})
        if not isinstance(raw, dict):
            raise ValueError("projects must be a table")

        seen: dict[Path, str] = {}
        projects: list[Project] = []
        for label in sorted(raw):
            values = raw[label]
            project = split_project(label)
            if project is None:
                raise ValueError(f"project key must have <group>/<project> form: {label}")
            if not isinstance(values, list) or not values:
                raise ValueError(f"project has no paths: {label}")

            roots = []
            for value in values:
                if not isinstance(value, str):
                    raise ValueError(f"project path must be a string: {label}")
                root = expand_path(value, home)
                if other := seen.get(root):
                    raise ValueError(f"path belongs to both {other} and {label}: {root}")
                seen[root] = label
                roots.append(root)
            group, name = project
            projects.append(Project(group, name, roots))
        return cls(projects)

    def map_workdir(self, workdir: str, by: StatsBy) -> str:
        if by is StatsBy.WORKDIR:
            return workdir
        project = self.find_project(workdir)
        if by is StatsBy.PROJECT and project:
            return f"{project.group}/{project.name}"
        if by is StatsBy.PROJECT:
            return workdir
        return project.group if project else "@unmapped"

    def find_project(self, workdir: str) -> Project | None:
        if workdir in self._cache:
            return self._cache[workdir]
        path = Path(workdir)
        matches = (
            (len(root.parts), project)
            for project in self.projects
            for root in project.roots
            if path == root or root in path.parents
        )
        project = max(matches, key=lambda x: x[0], default=(0, None))[1]
        self._cache[workdir] = project
        return project


def split_project(value: str) -> tuple[str, str] | None:
    parts = value.split("/")
    return (parts[0], parts[1]) if len(parts) == 2 and all(parts) else None


def expand_path(value: str, home: Path | None = None) -> Path:
    if value == "~":
        if home is None:
            raise ValueError("cannot expand ~ without HOME")
        path = home
    elif value.startswith("~/"):
        if home is None:
            raise ValueError("cannot expand ~ without HOME")
        path = home / value[2:]
    else:
        path = Path(value)
    if not path.is_absolute():
        raise ValueError(f"project path must be absolute: {value}")
    return path


def home_dir() -> Path | None:
    value = os.environ.get("HOME") or os.environ.get("USERPROFILE")
    return Path(value) if value else None


def config_path(home: Path | None = None) -> Path | None:
    if "CCTRAIL_CONFIG" in os.environ:
        return Path(os.environ["CCTRAIL_CONFIG"])

    root = os.environ.get("XDG_CONFIG_HOME")
    if root is not None:
        root = Path(root)
    else:
        home = home or home_dir()
        root = home / ".config" if home else None

    return root / "cctrail/config.toml" if root else None
