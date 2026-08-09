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


def normalized_path_list(value: object, error: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not value:
        raise ConfigError(error)

    paths = []
    for item in value:
        if not isinstance(item, str) or not item.strip():
            raise ConfigError(error)
        paths.append(normalize_path(item))
    return tuple(paths)


def register_exact_paths(paths: tuple[str, ...], seen: set[str], error: str) -> None:
    for path in paths:
        if "*" in path:
            continue
        if path in seen:
            raise ConfigError(f"{error}: {path}")
        seen.add(path)


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
            paths = normalized_path_list(
                raw_paths,
                f"project alias {name} needs at least one path",
            )
            register_exact_paths(paths, exact_paths, "project path appears in multiple aliases")
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
            projects = normalized_path_list(
                raw_projects,
                f"project group {name} needs at least one project",
            )
            register_exact_paths(projects, exact_projects, "project appears in multiple groups")
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
            return normalized_path_list(
                raw_paths,
                f"sources.{provider} needs at least one path",
            )

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
