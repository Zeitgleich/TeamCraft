# TeamCraft UI – English (source language)
# Search and the quick switcher.

## Top bar and account menu
search-placeholder = Search
search-in = Search in { $name }
search-scope-clear = Search everywhere

## Quick switcher
switcher-title = Search
switcher-input-label = Search people, threads, channels, spaces and messages
switcher-placeholder = Search people, channels and messages…
switcher-placeholder-filtered = Search
switcher-active-filters = Active filters
search-chip-in-channel = In this conversation
search-chip-from-me = From me
search-chip-mentions-me = Mentions me
search-chip-link = Has a link
search-chip-threads = Only threads
search-chip-last-week = Last 7 days
search-chip-more = More filters
switcher-results = Results
switcher-empty = No results.
switcher-group-recent = Recent
switcher-group-people = People
switcher-group-threads = Threads
switcher-group-channels = Channels
switcher-group-spaces = Spaces
switcher-group-messages = Messages
switcher-context-person = Person
switcher-context-thread = Thread in { $channel }
switcher-context-direct = Direct message
switcher-context-group = Group message
switcher-context-space = Space
switcher-context-join = Join space
switcher-all-messages = All messages for “{ $query }”
switcher-hint-navigate = navigate
switcher-hint-open = open
switcher-hint-close = close

## Search
switcher-search-messages = search messages
search-title = Search
search-input-label = Search messages
search-input-placeholder = Search messages, or add filters…
search-filters-label = Add a filter
search-filter-from = From
search-filter-in = In channel
search-filter-file = Has file
search-filter-link = Has link
search-filter-before = Before
search-filter-after = After
search-filter-thread = In thread
search-active-filters = Active filters
search-remove-filter = Remove filter { $filter }
search-invalid = can't be read
search-invalid-hint = Some filters can't be read and are ignored. Dates look like 2026-10-31.
search-syntax = Words must all match. Use "quotes" for a phrase, from:name, in:channel, space:name, has:file, has:link, before:2026-10-31, after:2026-10-01 and in-thread.
search-start = Type words or add a filter to search.
search-pending = Searching…
search-empty = Nothing found. Try fewer words or remove a filter.
search-count = { $count ->
    [one] { $count } result
   *[other] { $count } results
  }
search-save = Save search
search-saved-title = Saved searches
search-saved-empty = Save a search to run it again with one click.
search-saved-delete = Delete saved search { $name }
search-mentions-title = Mentions
search-mentions-open = Recent mentions
search-reply-badge = Reply
search-open-conversation = Open conversation
search-open-thread = Open thread

## Activity, last seen and call history
# Spaces: moving channels, muting, scoped search, grouped sidebar, channel retention
search-scope-label = Search in
search-scope-all = All spaces
search-filter-space = In space
