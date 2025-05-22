let flatpak_packages =  {
  "remotes": [ # useless as of now, will be useful for rebuild command, optional
    {
      "package": "flathub",
      "url": "https://dl.flathub.org/repo/flathub.flatpakrepo",
    }
  ]
  "pinned": [ # pinned user flatpak runtimes, optional
    {
      "package": "org.gtk.Gtk3theme.adw-gtk3",
      "branch": "stable", # branch of the pinned package
      "arch": "x86_64",
      "post_hook": {|| echo foo}, # executed after the pin is installed
    },
  ]
  "packages": [ # only user flatpaks
    {
       "package": "com.github.flxzt.rnote",
       "remote": "flathub", # flatpak remote from which to install the package
                            # must correspond to a valid remote on the system
                            # optional
    },
  ]
}
