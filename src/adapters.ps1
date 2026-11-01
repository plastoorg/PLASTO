
    $out = @()
    foreach ($a in Get-NetAdapter | Where-Object { $_.Status -eq 'Up' }) {
      $s = Get-DnsClientServerAddress -InterfaceIndex $a.ifIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue
      $k = "HKLM:\SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\$($a.InterfaceGuid)"
      $ns = (Get-ItemProperty -Path $k -Name NameServer -ErrorAction SilentlyContinue).NameServer
      $out += [pscustomobject]@{
        index   = [int]$a.ifIndex
        alias   = [string]$a.InterfaceAlias
        guid    = [string]$a.InterfaceGuid
        dhcp    = [bool][string]::IsNullOrWhiteSpace($ns)
        servers = @($s.ServerAddresses)
      }
    }
    ConvertTo-Json -InputObject @($out) -Compress -Depth 4