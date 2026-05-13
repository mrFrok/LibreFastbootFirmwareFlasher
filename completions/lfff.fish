# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_lfff_global_optspecs
	string join \n v/verbose h/help V/version
end

function __fish_lfff_needs_command
	# Figure out if the current invocation already has a command.
	set -l cmd (commandline -opc)
	set -e cmd[1]
	argparse -s (__fish_lfff_global_optspecs) -- $cmd 2>/dev/null
	or return
	if set -q argv[1]
		# Also print the command, so this can be used to figure out what it is.
		echo $argv[1]
		return 1
	end
	return 0
end

function __fish_lfff_using_subcommand
	set -l cmd (__fish_lfff_needs_command)
	test -z "$cmd"
	and return 1
	contains -- $cmd[1] $argv
end

complete -c lfff -n "__fish_lfff_needs_command" -s v -l verbose -d 'Enable debug logging'
complete -c lfff -n "__fish_lfff_needs_command" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_needs_command" -s V -l version -d 'Print version'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "deps" -d 'Install and verify external dependencies'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "download" -d 'Download firmware via OTA link (supports 4PDA redirects)'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "extract" -d 'Extract a firmware .zip archive'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "devices" -d 'List connected devices, run pre-flash diagnostics'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "arb" -d 'Check Anti-Rollback version of a firmware'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "flash" -d 'Flash an extracted firmware directory (or source build with --source)'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "completions" -d 'Generate shell completion script'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "flash-partition" -d 'Flash a single .img file to a specific partition'
complete -c lfff -n "__fish_lfff_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c lfff -n "__fish_lfff_using_subcommand deps" -l check -d 'Only check, do not install anything'
complete -c lfff -n "__fish_lfff_using_subcommand deps" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand download" -s o -l output -d 'Directory to save the firmware (default: current directory)' -r -F
complete -c lfff -n "__fish_lfff_using_subcommand download" -s c -l connections -d 'Number of parallel connections for aria2c' -r
complete -c lfff -n "__fish_lfff_using_subcommand download" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand extract" -s o -l output -d 'Output directory (prompted if not given)' -r -F
complete -c lfff -n "__fish_lfff_using_subcommand extract" -s p -l partitions -d 'Comma-separated partitions to extract' -r
complete -c lfff -n "__fish_lfff_using_subcommand extract" -l checksum -d 'Expected SHA-256 checksum of the archive' -r
complete -c lfff -n "__fish_lfff_using_subcommand extract" -l list -d 'List archive contents without extracting'
complete -c lfff -n "__fish_lfff_using_subcommand extract" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand devices" -s s -l serial -d 'Target a specific device by serial number' -r
complete -c lfff -n "__fish_lfff_using_subcommand devices" -l check -d 'Run full pre-flash diagnostics'
complete -c lfff -n "__fish_lfff_using_subcommand devices" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand arb" -l xbl -d 'Direct path to xbl_config.img' -r -F
complete -c lfff -n "__fish_lfff_using_subcommand arb" -l firmware-dir -d 'Extracted firmware directory (xbl_config.img located automatically)' -r -F
complete -c lfff -n "__fish_lfff_using_subcommand arb" -s s -l serial -d 'Target a specific device by serial number' -r
complete -c lfff -n "__fish_lfff_using_subcommand arb" -l device -d 'Also read ARB version from connected device and compare'
complete -c lfff -n "__fish_lfff_using_subcommand arb" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand flash" -l source -d 'Android source build output directory (skips ARB check)' -r -F
complete -c lfff -n "__fish_lfff_using_subcommand flash" -s s -l serial -d 'Target a specific device by serial number' -r
complete -c lfff -n "__fish_lfff_using_subcommand flash" -l dry-run -d 'Detect images and run checks without flashing'
complete -c lfff -n "__fish_lfff_using_subcommand flash" -l skip-xbl-abl -d 'Skip xbl and abl partitions during flashing'
complete -c lfff -n "__fish_lfff_using_subcommand flash" -l skip-preloader -d 'Skip preloader partition during flashing'
complete -c lfff -n "__fish_lfff_using_subcommand flash" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand completions" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand flash-partition" -l firmware-dir -d 'Extracted firmware directory to search for the partition image' -r -F
complete -c lfff -n "__fish_lfff_using_subcommand flash-partition" -s p -l partition -d 'Partition name override (default: image filename stem)' -r
complete -c lfff -n "__fish_lfff_using_subcommand flash-partition" -l slot -d 'Slot(s) to flash: a, b, or a,b (default: both)' -r
complete -c lfff -n "__fish_lfff_using_subcommand flash-partition" -s s -l serial -d 'Target a specific device' -r
complete -c lfff -n "__fish_lfff_using_subcommand flash-partition" -l no-ab -d 'Flash without slot suffix (for non-A/B partitions)'
complete -c lfff -n "__fish_lfff_using_subcommand flash-partition" -l dry-run -d 'Show what would be flashed without actually flashing'
complete -c lfff -n "__fish_lfff_using_subcommand flash-partition" -s h -l help -d 'Print help'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "deps" -d 'Install and verify external dependencies'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "download" -d 'Download firmware via OTA link (supports 4PDA redirects)'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "extract" -d 'Extract a firmware .zip archive'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "devices" -d 'List connected devices, run pre-flash diagnostics'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "arb" -d 'Check Anti-Rollback version of a firmware'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "flash" -d 'Flash an extracted firmware directory (or source build with --source)'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "completions" -d 'Generate shell completion script'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "flash-partition" -d 'Flash a single .img file to a specific partition'
complete -c lfff -n "__fish_lfff_using_subcommand help; and not __fish_seen_subcommand_from deps download extract devices arb flash completions flash-partition help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
