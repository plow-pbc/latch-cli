#!/bin/zsh
# In the guest: the Rules page on the synthetic latch, driven and recorded by rig, light then dark.
cd ~/q; O=~/q/out; mkdir -p $O; log() { echo "$*" | tee -a $O/run.log; }
mkdir -p ~/q/p && tar xzf ~/q/spike.tgz -C ~/q/p
osascript -e "set volume output muted true"
m=$(osascript -e "output muted of (get volume settings)")
log "audio: muted $m"
[ "$m" = true ] || { log "not muted: stopping before any input"; exit 1; }
log "display: $(~/q/p/modes 1600x1000 2>&1 | tail -1)"
defaults write com.apple.CrashReporter DialogType none
log "macOS $(sw_vers -productVersion) ($(sw_vers -buildVersion))"
# Finder first, so capture is allowed once (as the shell spike's runs do).
open ~; sleep 3; F=$(pgrep -x Finder)
~/q/p/rig $F $O/warm < /dev/null > /dev/null 2>&1 & W=$!
sleep 3; U=$(pgrep -x UserNotificationCenter); [ -n "$U" ] && printf 'x\tpress AXButton|Allow\n' | ~/q/p/vis post $U x 800
wait $W
plan() {
  print "launch\twait 3000"; print "x\tmove 40,140"; print "x\twait 1000"; print "x\tstill home"
  print "to-agents\tclick AXStaticText|Agents"
  print "to-rules\tclick AXStaticText|Rules"; print "x\tstill rules"; print "x\tscreen rules"
  print "to-connections\tclick AXStaticText|Connections"
  print "back-to-rules\tclick AXStaticText|Rules"
  print "to-host\tclick AXStaticText|sidecar"
  print "rules-again\tclick AXStaticText|Rules"
  print "pick-row\tclick AXStaticText|scout may run builds"; print "x\tfocus"
  print "arrow-down\tkey down"; print "x\tfocus"; print "x\tscreen down-moved-row"
  print "arrow-up\tkey up"; print "x\tfocus"
  print "edit\tkey return"; print "x\tscreen edit"
  print "agents-menu\tclickat 796,503"; print "x\tscreen edit-agents-menu"
  print "close-menu\tkey escape"
  print "delete\tclickat 413,672"; print "x\tscreen edit-delete-asks"
  print "keep\tkey escape"; print "x\tscreen edit-delete-cancelled"
  print "close-edit\tkey escape"
  print "pick-again\tclick AXStaticText|helper may run builds"
  print "backspace\tkey delete"; print "x\tscreen list-delete-asks"
  print "no\tkey escape"
  print "new-rule\tkey cmd-n"; print "x\tscreen new-rule"
  print "cancel-new\tkey escape"
  print "filter\tclick AXPopUpButton|Any"; print "x\tscreen agent-filter-menu"
  print "filter-pick\tclick AXMenuItem|scout"; print "x\tfocus"; print "x\tscreen filtered-scout"
  print "try\tclick AXButton|Try a Request"
  print "examples\tclick AXMenuButton|Examples"; print "x\tscreen try-examples"
  print "example\tclick AXMenuItem|Find unread email"
  print "run-try\tkey return"; print "x\twait 2500"; print "x\tscreen try-verdict"
  print "done\tkey escape"
  print "home\tclick AXStaticText|studio"; print "x\tstill home-again"
}
for mode in light dark; do
  ARGS=(); [ $mode = dark ] && ARGS=(--env RULES_DARK=1)
  # The guest's own first-boot banners, off the shots.
  killall NotificationCenter 2>/dev/null; sleep 1
  L=$O/$mode; mkdir -p $L
  open ~/q/p/Rules.app --stderr $L/app.stderr $ARGS
  for i in {1..20}; do sleep 1; P=$(pgrep -x Rules); [ -n "$P" ] && ~/q/p/vis windows 0 | grep -q " Rules " && break; done
  log "$mode: pid $P; window $(~/q/p/vis windows 0 | grep ' Rules ' | head -1)"
  plan | ~/q/p/rig $P $L; r=$?
  log "$mode: rig exit $r $(cat $L/ABORTED 2>/dev/null)"
  kill $P 2>/dev/null; sleep 2
  [ $r -eq 3 ] && break
done
log done
