# Who latch's macOS app is, for the scripts and workflows that build, sign
# and check it: sourced (`. latch/macos/identity.sh`), never run. Full Disk
# Access and the Keychain are granted to this bundle id signed by this
# team, so a change here loses both on every Mac it installs to.
LATCH_APP_NAME='Plow Latch'
LATCH_BUNDLE_ID='co.plow.latch.cli'
LATCH_TEAM_ID='3559PD337Z'
LATCH_SIGNING_IDENTITY='Developer ID Application: The Plow Collective, Inc (3559PD337Z)'
# The Messages reader, signed by the same team under its own id.
LATCH_READER_ID='co.plow.latch.messages'
