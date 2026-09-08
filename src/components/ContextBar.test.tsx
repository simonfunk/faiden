import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { ContextBar } from './ContextBar'
import { directory } from '../test/fakeApi'

const noop = vi.fn()

describe('ContextBar', () => {
  it('offers to bind a directory without implying one is required', () => {
    render(<ContextBar workdir={null} context={null} busy={false} onPick={noop} onClear={noop} onRefresh={noop} />)

    expect(screen.getByRole('button', { name: /choose directory/i })).toBeInTheDocument()
    expect(screen.getByText(/no directory bound/i)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /clear directory/i })).not.toBeInTheDocument()
  })

  it('labels the path as a user selection, not as an observed working directory', () => {
    render(
      <ContextBar
        workdir="/tmp/project"
        context={directory('/tmp/project')}
        busy={false}
        onPick={noop}
        onClear={noop}
        onRefresh={noop}
      />,
    )

    expect(screen.getByText('/tmp/project')).toBeInTheDocument()
    const note = screen.getByTestId('provenance-note').textContent ?? ''
    expect(note.toLowerCase()).toContain('not an observed process working directory')
  })

  it('reports a plain directory as having no repository rather than as an error', () => {
    render(
      <ContextBar
        workdir="/tmp/plain"
        context={directory('/tmp/plain')}
        busy={false}
        onPick={noop}
        onClear={noop}
        onRefresh={noop}
      />,
    )
    expect(screen.getByText(/not a git repository/i)).toBeInTheDocument()
  })

  it('labels an unavailable Git probe as unavailable instead of a non-repository', () => {
    render(
      <ContextBar
        workdir="/tmp/timed-out"
        context={{ ...directory('/tmp/timed-out'), gitUnavailable: 'Git inspection timed out' }}
        busy={false}
        onPick={noop}
        onClear={noop}
        onRefresh={noop}
      />,
    )

    expect(screen.getByText(/git status unavailable/i)).toBeInTheDocument()
    expect(screen.queryByText(/not a git repository/i)).not.toBeInTheDocument()
  })

  it('shows branch, short head and a clean state', () => {
    render(
      <ContextBar
        workdir="/tmp/repo"
        context={directory('/tmp/repo', {
          git: {
            repoRoot: '/tmp/repo',
            branch: 'main',
            headShort: 'a1b2c3d',
            detached: false,
            unborn: false,
            bare: false,
            isLinkedWorktree: false,
            dirty: false,
            changedFiles: 0,
          },
        })}
        busy={false}
        onPick={noop}
        onClear={noop}
        onRefresh={noop}
      />,
    )

    expect(screen.getByText('main')).toBeInTheDocument()
    expect(screen.getByText('a1b2c3d')).toBeInTheDocument()
    expect(screen.getByText(/clean/i)).toBeInTheDocument()
    expect(screen.queryByText(/worktree/i)).not.toBeInTheDocument()
  })

  it('labels a detached head, a dirty tree and a linked worktree', () => {
    render(
      <ContextBar
        workdir="/tmp/wt"
        context={directory('/tmp/wt', {
          git: {
            repoRoot: '/tmp/wt',
            branch: null,
            headShort: 'deadbee',
            detached: true,
            unborn: false,
            bare: false,
            isLinkedWorktree: true,
            dirty: true,
            changedFiles: 3,
          },
        })}
        busy={false}
        onPick={noop}
        onClear={noop}
        onRefresh={noop}
      />,
    )

    expect(screen.getByText(/detached head/i)).toBeInTheDocument()
    expect(screen.getByText(/3 changed files/i)).toBeInTheDocument()
    expect(screen.getByText(/linked worktree/i)).toBeInTheDocument()
  })

  it('says an unborn repository has no commit instead of inventing a head', () => {
    render(
      <ContextBar
        workdir="/tmp/new"
        context={directory('/tmp/new', {
          git: {
            repoRoot: '/tmp/new',
            branch: 'main',
            headShort: null,
            detached: false,
            unborn: true,
            bare: false,
            isLinkedWorktree: false,
            dirty: false,
            changedFiles: 0,
          },
        })}
        busy={false}
        onPick={noop}
        onClear={noop}
        onRefresh={noop}
      />,
    )
    expect(screen.getByText(/no commits yet/i)).toBeInTheDocument()
  })

  it('warns when a bound directory no longer exists', () => {
    render(
      <ContextBar
        workdir="/tmp/gone"
        context={directory('/tmp/gone', { exists: false, isDirectory: false })}
        busy={false}
        onPick={noop}
        onClear={noop}
        onRefresh={noop}
      />,
    )
    expect(screen.getByRole('alert')).toHaveTextContent(/no longer exists|missing/i)
  })
})
