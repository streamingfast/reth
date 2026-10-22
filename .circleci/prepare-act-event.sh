#!/usr/bin/env bash
set -euo pipefail

readonly OUTPUT_DIR="${ACT_EVENT_OUTPUT_DIR:-/tmp/act-event}"
readonly EVENT_PATH="${OUTPUT_DIR}/github-event.json"

mkdir -p "$OUTPUT_DIR"
rm -f "$EVENT_PATH"

: "${CIRCLE_SHA1:?missing CircleCI revision}"
: "${CIRCLE_PROJECT_USERNAME:?missing CircleCI project owner}"
: "${CIRCLE_PROJECT_REPONAME:?missing CircleCI project repository}"

repository="${CIRCLE_PROJECT_USERNAME}/${CIRCLE_PROJECT_REPONAME}"

if [[ "${CIRCLE_BRANCH:-}" == gh-readonly-queue/* ]]; then
    event_name="merge_group"
elif [[ -n "${CIRCLE_PULL_REQUEST:-}" ]]; then
    event_name="pull_request"
else
    event_name="push"
fi

case "$event_name" in
    pull_request)
        : "${CIRCLE_PULL_REQUEST:?pull-request trigger must provide CIRCLE_PULL_REQUEST}"
        pr_number="${CIRCLE_PULL_REQUEST##*/}"
        if [[ ! "$pr_number" =~ ^[1-9][0-9]*$ ]]; then
            echo "invalid pull-request URL: ${CIRCLE_PULL_REQUEST}" >&2
            exit 1
        fi

        pull_request_path="${OUTPUT_DIR}/pull-request.json"
        curl --fail --silent --show-error --location \
            -H "Accept: application/vnd.github+json" \
            -H "Authorization: Bearer ${MISE_GITHUB_TOKEN:?missing restricted read-only GitHub token}" \
            -H "X-GitHub-Api-Version: 2022-11-28" \
            "https://api.github.com/repos/${repository}/pulls/${pr_number}" > "$pull_request_path"

        event_head_sha="$(jq -er '.head.sha' "$pull_request_path")"
        if [[ "$event_head_sha" != "$CIRCLE_SHA1" ]]; then
            echo "PR head does not match CircleCI revision: ${CIRCLE_SHA1} != ${event_head_sha}" >&2
            exit 1
        fi

        jq '{
            action: "synchronize",
            number: .number,
            pull_request: .,
            repository: .base.repo,
            sender: .user
        }' "$pull_request_path" > "$EVENT_PATH"
        base_ref="$(jq -er '.base.ref | select(length > 0)' "$pull_request_path")"
        head_ref="$(jq -er '.head.ref | select(length > 0)' "$pull_request_path")"
        echo "act event: pull_request #${pr_number}, ${head_ref} -> ${base_ref}"
        ;;
    push)
        : "${CIRCLE_BRANCH:?push trigger must provide CIRCLE_BRANCH}"
        jq -n \
            --arg ref "refs/heads/${CIRCLE_BRANCH}" \
            --arg after "$CIRCLE_SHA1" \
            '{ref: $ref, after: $after}' > "$EVENT_PATH"
        echo "act event: push ${CIRCLE_BRANCH} at ${CIRCLE_SHA1}"
        ;;
    merge_group)
        : "${CIRCLE_BRANCH:?merge-queue trigger must provide CIRCLE_BRANCH}"
        if [[ ! "$CIRCLE_BRANCH" =~ ^gh-readonly-queue/(.+)/pr-([1-9][0-9]*)-[^/]+$ ]]; then
            echo "merge_group requires a gh-readonly-queue/<base>/pr-<number>-<suffix> branch, got: ${CIRCLE_BRANCH}" >&2
            exit 1
        fi
        base_ref="${BASH_REMATCH[1]}"

        if ! base_sha="$(git rev-parse "${CIRCLE_SHA1}^1" 2>/dev/null)"; then
            git fetch --no-tags --depth=2 origin "$CIRCLE_SHA1"
            base_sha="$(git rev-parse "${CIRCLE_SHA1}^1")"
        fi

        jq -n \
            --arg head_sha "$CIRCLE_SHA1" \
            --arg head_ref "refs/heads/${CIRCLE_BRANCH}" \
            --arg base_sha "$base_sha" \
            --arg base_ref "refs/heads/${base_ref}" \
            --arg repository "$repository" \
            --arg repository_name "$CIRCLE_PROJECT_REPONAME" \
            --arg repository_owner "$CIRCLE_PROJECT_USERNAME" \
            '{
                action: "checks_requested",
                merge_group: {
                    head_sha: $head_sha,
                    head_ref: $head_ref,
                    base_sha: $base_sha,
                    base_ref: $base_ref
                },
                repository: {
                    full_name: $repository,
                    name: $repository_name,
                    owner: {login: $repository_owner}
                }
            }' > "$EVENT_PATH"
        echo "act event: merge_group ${CIRCLE_BRANCH} -> ${base_ref} at ${base_sha}"
        ;;
esac
