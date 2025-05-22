let arch_packages =  {
  packages: [
  {
    "package": "7zip",
    "post_hook": {|| touch ($env.HOME + "/foo.txt") }
    
  },
  ]
}

