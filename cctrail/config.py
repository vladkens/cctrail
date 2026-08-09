import os
import re
import tomllib
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path


class ConfigError(ValueError):
    pass


def config_path() -> Path:
    local = Path.cwd() / "cctrail.toml"
    if local.is_file():
        return local

    root = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config")).expanduser()
    return root / "cctrail.toml"


def normalize_path(value: str) -> str:
    return os.path.normpath(os.path.expanduser(value)).replace(os.sep, "/")


@lru_cache(maxsize=4096)
def match_path(path: str, pattern: str) -> bool:
    expression = ""
    index = 0
    while index < len(pattern):
        if pattern[index:] == "/**":
            expression += "(?:/.*)?"
            index += 3
        elif pattern[index : index + 2] == "**":
            if index + 2 < len(pattern) and pattern[index + 2] == "/":
                expression += "(?:.*/)?"
                index += 3
            else:
                expression += ".*"
                index += 2
        elif pattern[index] == "*":
            expression += "[^/]*"
            index += 1
        else:
            expression += re.escape(pattern[index])
            index += 1
    return re.fullmatch(expression, path) is not None


@dataclass(frozen=True)
class ProjectAlias:
    name: str
    paths: tuple[str, ...]


@dataclass(frozen=True)
class ProjectGroup:
    name: str
    projects: tuple[str, ...]


@dataclass(frozen=True)
class Config:
    aliases: tuple[ProjectAlias, ...] = ()
    groups: tuple[ProjectGroup, ...] = ()
    claude_sources: tuple[str, ...] | None = None
    codex_sources: tuple[str, ...] | None = None

    @classmethod
    def load(cls, path: Path | None = None, *, required: bool = False) -> "Config":
        path = config_path() if path is None else path
        try:
            with path.open("rb") as handle:
                value = tomllib.load(handle)
        except FileNotFoundError:
            if required:
                raise ConfigError(f"config file not found: {path}") from None
            return cls()
        except (OSError, tomllib.TOMLDecodeError) as exc:
            raise ConfigError(f"cannot read {path}: {exc}") from exc

        raw_aliases = value.get("aliases", {})
        if not isinstance(raw_aliases, dict):
            raise ConfigError("aliases must be a table")

        aliases = []
        exact_paths: set[str] = set()
        for raw_name, raw_paths in raw_aliases.items():
            name = str(raw_name).strip()
            if not name:
                raise ConfigError("project alias names cannot be empty")
            if (
                not isinstance(raw_paths, list)
                or not raw_paths
                or not all(isinstance(item, str) and item.strip() for item in raw_paths)
            ):
                raise ConfigError(f"project alias {name} needs at least one path")

            paths = tuple(normalize_path(item) for item in raw_paths)
            for member in paths:
                if "*" not in member:
                    if member in exact_paths:
                        raise ConfigError(f"project path appears in multiple aliases: {member}")
                    exact_paths.add(member)
            aliases.append(ProjectAlias(name, paths))

        raw_groups = value.get("groups", {})
        if not isinstance(raw_groups, dict):
            raise ConfigError("groups must be a table")

        groups = []
        exact_projects: set[str] = set()
        for raw_name, raw_projects in raw_groups.items():
            name = str(raw_name).strip()
            if not name:
                raise ConfigError("project group names cannot be empty")
            if name == "ungrouped":
                raise ConfigError("ungrouped is reserved for projects without a group")
            if (
                not isinstance(raw_projects, list)
                or not raw_projects
                or not all(isinstance(item, str) and item.strip() for item in raw_projects)
            ):
                raise ConfigError(f"project group {name} needs at least one project")

            projects = tuple(normalize_path(item) for item in raw_projects)
            for project in projects:
                if "*" not in project:
                    if project in exact_projects:
                        raise ConfigError(f"project appears in multiple groups: {project}")
                    exact_projects.add(project)
            groups.append(ProjectGroup(name, projects))

        raw_sources = value.get("sources", {})
        if not isinstance(raw_sources, dict):
            raise ConfigError("sources must be a table")
        unknown_sources = {str(key) for key in raw_sources} - {"claude", "codex"}
        if unknown_sources:
            raise ConfigError(f"unknown sources: {', '.join(sorted(unknown_sources))}")

        def sources(provider: str) -> tuple[str, ...] | None:
            raw_paths = raw_sources.get(provider)
            if raw_paths is None:
                return None
            if (
                not isinstance(raw_paths, list)
                or not raw_paths
                or not all(isinstance(item, str) and item.strip() for item in raw_paths)
            ):
                raise ConfigError(f"sources.{provider} needs at least one path")
            return tuple(normalize_path(item) for item in raw_paths)

        return cls(
            aliases=tuple(aliases),
            groups=tuple(groups),
            claude_sources=sources("claude"),
            codex_sources=sources("codex"),
        )

    @lru_cache(maxsize=256)
    def resolve_project(self, project: str) -> str:
        normalized = normalize_path(project)
        matches = [
            alias.name
            for alias in self.aliases
            if any(match_path(normalized, pattern) for pattern in alias.paths)
        ]
        if len(matches) > 1:
            raise ConfigError(f"project {project} matches multiple aliases: {', '.join(matches)}")
        return matches[0] if matches else project

    @lru_cache(maxsize=256)
    def resolve_group(self, project: str) -> str:
        normalized = normalize_path(project)
        matches = [
            group.name
            for group in self.groups
            if any(match_path(normalized, pattern) for pattern in group.projects)
        ]
        if len(matches) > 1:
            raise ConfigError(f"project {project} matches multiple groups: {', '.join(matches)}")
        return matches[0] if matches else "ungrouped"
