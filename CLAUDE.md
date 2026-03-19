# TERMS:

- input: data about hardware, connection status, etc.
- operation: an action that is taken on the system

# RULES:

- tests: unless said otherwise, they perform with simulated input and produce output on the operations that would be performed. They never affect the running system.
- running tests: use the make tasks every time.
- tests should always include the linting checks
- lint checks should be a rust community standard of linters, run as the `lint` make tasks

# DESIGN:

A rust-based text user interface for installing Town OS. It should
be presented when the user would normally be expected to provision disks.

This text user interface should work similarly to nmtui but should make the
focus getting on the internet. If there is a clear path to the internet, it
should just ask if the user wants to change it but show them how it will
connect. That way, if they need to configure wireless they can, but if there is
an option to connect on the wire already, it should just ask.

This text interface should also ask the user what to do with the disks that
exist on the system. Offer configurations:

Group storage by make and model automatically, and offer the user a choice of
which group to provision.

Then offer RAID options with explanations:

- 1 disk - all one drive
- 2 disks - mirror
- 3+ Disks - raidz

This should be testable -- a manifest of actions taken in this case instead of
actually taking them. Likewise, inputs for the available wifi networks, ethernet
configurations, and storage options should be able to be accepted as a manifest
for how to present the installer. The result is an installer that can totally
be fed hardware configurations and let the user mess around in this virtual
environment, and then generate a result of actions to be taken that can be
analyzed later.

Write a set of fixtures that runs through the installer setting up common
hardware and enviornment configurations:

- ethernet + 4 disks all same
- ethernet + 1 disk
- wifi + 1 disk
- wifi in crowded neighborhood + 1 disk
- wifi + ethernet + 4 disks
- wifi + ethernet + 1 disk
- wifi + dead ethernet + 1 disk
- wifi + dead ethernet + 4 disks

Now, write a consistent plan of operations:

- turning a network device on
- scanning a wifi network
- checking for link availability
- performing wifi authentication checks
- supporting automatic wifi configuration, such as qr codes
- checking for ip address
- checking for internet routability
- checking for upstream router
- configuring dhcp for interface
- configuring wifi ssid and authentication for interface
- receiving a list of available ssids with signal strength, etc.
- wifi connection timeout
- wifi auth error
- selection of primary interface - other interfaces should be shut down
- DNS resolution works
- network completely online

Then, I want you to mix these fixtures, and selections of the options available
in the fixtures (assume that the ssid may or may not be connected to when
dealing with names and passwords) with the operations that should be run.

Examples:

- selecting a wifi network from a list of them.
- refreshing the list with a new scan. the list should change.
- connecting to a wifi network and being prompted for a password.
- entering an incorrect password and getting an error back.
- connecting to a wifi network that has a signal timeout (short).
- successful connection to a wifi network with ip provisioning.
- locating and ip provisioning the correct default device:
    - first, connected ethernet
    - second, available wifi devices
    - third, ask user after listing available devices

Please ensure all these mixes are tested. Please provide them separate from the
code so they can be manipulated independently. Inputs should generate a series
of operations; running the inputs should generate the list of operations.

Inputs should drive interactions in the TUI which may involve internal state, or the generation of operations (such as move a file, talk to the network, etc) that should be executed.

The result would be that in a real scenario, those operations will be evaluated immediately, and their results would be fed back as error or state changes, which then might interrupt the input for prepending, such as a ssid list, to wifi access point selection, to password entry, to network negotiation and online status, including DNS resolution of e.g. example.com, but resulting in a full series of inputs and any errors states in-between, and the operations that would have been performed in the series of errors to get to a final state of "installed" or "aborted". The option to reboot the machine should also be available.

Please ensure any other additional functionality is tested.

- don't commit or push unless I tell you to
